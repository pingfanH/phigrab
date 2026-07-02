//! DGHub 判定联动 —— phira 进程内直接实现 DGHub 插件协议 (SDK v1)。
//!
//! 设计文档见 `dghub/PHIRA_INTEGRATION.md`。
//!
//! ## 连接状态反馈
//!
//! 后台任务把连接/断开事件写入全局队列 [`drain_events`]。
//! 上层（song.rs）每帧轮询，发现事件后通过 `show_message` 弹窗告知用户。

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use prpr::scene::UpdateFn;
use serde_json::{json, Value};
use std::sync::{atomic::AtomicU8, atomic::Ordering, Arc, Mutex};
use tokio::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

/// 四档判定，对应 `prpr::judge::Judgement`（`counts` 数组下标顺序）。
#[derive(Clone, Copy, Debug)]
pub enum Grade {
    Perfect,
    Good,
    Bad,
    Miss,
}

impl Grade {
    fn from_index(i: usize) -> Self {
        match i {
            0 => Grade::Perfect,
            1 => Grade::Good,
            2 => Grade::Bad,
            _ => Grade::Miss,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Grade::Perfect => "Perfect",
            Grade::Good => "Good",
            Grade::Bad => "Bad",
            Grade::Miss => "Miss",
        }
    }
}

// ---------------------------------------------------------------------------
// 连接事件（游戏线程轮询用）
// ---------------------------------------------------------------------------

/// 后台任务推入全局队列的连接状态变化。
#[derive(Clone, Debug)]
pub enum DghubEvent {
    Connected,
    Disconnected(String),
}

/// 连接状态。
const STATUS_CONNECTING: u8 = 1;
const STATUS_CONNECTED: u8 = 2;

static DGHUB_EVENTS: std::sync::LazyLock<Mutex<Vec<DghubEvent>>> = std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// 全局连接状态（供 settings 等页面读取）。
static DGHUB_CONNECTION_STATUS: std::sync::LazyLock<Arc<AtomicU8>> = std::sync::LazyLock::new(|| Arc::new(AtomicU8::new(0)));

/// 全局共享的 grade 发送端 + 重连参数。
/// `spawn` 写入，`build_update_fn` 和 `request_reconnect` 读取/替换。
static DGHUB_SESSION: std::sync::LazyLock<Mutex<Option<DghubSession>>> = std::sync::LazyLock::new(|| Mutex::new(None));

struct DghubSession {
    host: String,
    port: u16,
    token: Option<String>,
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<Grade>>>>,
}

/// 消费所有待处理事件。游戏线程每帧调用。
pub fn drain_events() -> Vec<DghubEvent> {
    std::mem::take(&mut *DGHUB_EVENTS.lock().unwrap())
}

/// 连接状态：0=未连接，1=连接中，2=已连接。
pub fn connection_status() -> u8 {
    DGHUB_CONNECTION_STATUS.load(Ordering::Relaxed)
}

/// 尝试重连：复用共享 sender（若有），起新后台任务。
pub fn request_reconnect() {
    let session = DGHUB_SESSION
        .lock()
        .unwrap()
        .as_ref()
        .map(|session| (session.host.clone(), session.port, session.token.clone(), Arc::clone(&session.tx)));

    if let Some((host, port, token, tx_shared)) = session {
        info!("dghub: reconnect requested -> {host}:{port}");

        // 为新任务建一对新 channel；替换共享 sender
        let (new_tx, rx) = mpsc::unbounded_channel();
        *tx_shared.lock().unwrap() = Some(new_tx);

        let mapping = Mapping::from_config(&crate::get_data().config);
        let status: Arc<AtomicU8> = Arc::new(AtomicU8::new(STATUS_CONNECTING));
        DGHUB_CONNECTION_STATUS.store(STATUS_CONNECTING, Ordering::Relaxed);
        tokio::spawn(async move {
            match run(host, port, token, mapping, rx, &status).await {
                Ok(()) => info!("dghub: reconnect session ended"),
                Err(err) => warn!("dghub: reconnect error: {err:?}"),
            }
            if status.load(Ordering::Relaxed) == STATUS_CONNECTED {
                DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Disconnected("连接断开".into()));
            }
            status.store(0, Ordering::Relaxed);
            DGHUB_CONNECTION_STATUS.store(0, Ordering::Relaxed);
        });
    } else {
        let (enabled, host, port, token) = {
            let cfg = &crate::get_data().config;
            (cfg.dghub_enable, cfg.dghub_host.clone(), cfg.dghub_port, normalize_token(&cfg.dghub_token))
        };
        if enabled {
            info!("dghub: reconnect requested without session; spawning from config -> {host}:{port}");
            let _ = spawn(host, port, token);
        } else {
            warn!("dghub: reconnect requested but DGHub is disabled");
        }
    }
}

pub fn normalize_token(token: &str) -> Option<String> {
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

// ---------------------------------------------------------------------------
// 映射 (从 phira Config 构建，不再依赖 DGHub 配置页)
// ---------------------------------------------------------------------------

/// 单档判定的触发参数。
#[derive(Clone)]
struct GradeCfg {
    enable: bool,
    delta: i32,
    duration: f64,
    preset: String,
}

/// 判定 → trigger 的完整映射。从 `prpr::config::Config` 构建，
/// 用户直接在 phira 设置页修改全部参数。
#[derive(Clone)]
struct Mapping {
    channel: String,
    throttle_ms: u32,
    perfect: GradeCfg,
    good: GradeCfg,
    bad: GradeCfg,
    miss: GradeCfg,
}

impl Mapping {
    fn from_config(cfg: &prpr::config::Config) -> Self {
        Mapping {
            channel: cfg.dghub_channel.clone(),
            throttle_ms: cfg.dghub_throttle_ms,
            miss: GradeCfg {
                enable: cfg.dghub_miss_enable,
                delta: cfg.dghub_miss_strength as i32,
                duration: cfg.dghub_miss_duration,
                preset: cfg.dghub_miss_preset.clone(),
            },
            bad: GradeCfg {
                enable: cfg.dghub_bad_enable,
                delta: cfg.dghub_bad_strength as i32,
                duration: cfg.dghub_bad_duration,
                preset: cfg.dghub_bad_preset.clone(),
            },
            good: GradeCfg {
                enable: cfg.dghub_good_enable,
                delta: cfg.dghub_good_strength as i32,
                duration: cfg.dghub_good_duration,
                preset: cfg.dghub_good_preset.clone(),
            },
            perfect: GradeCfg {
                enable: cfg.dghub_perfect_enable,
                delta: cfg.dghub_perfect_strength as i32,
                duration: cfg.dghub_perfect_duration,
                preset: cfg.dghub_perfect_preset.clone(),
            },
        }
    }

    fn cfg(&self, g: Grade) -> &GradeCfg {
        match g {
            Grade::Perfect => &self.perfect,
            Grade::Good => &self.good,
            Grade::Bad => &self.bad,
            Grade::Miss => &self.miss,
        }
    }

    /// 应用 `config_changed` 增量 —— 用显式 key 名直接匹配。
    fn apply_one(&mut self, key: &str, v: &Value) {
        match key {
            "channel" => {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() {
                        self.channel = s.to_owned();
                    }
                }
            }
            "throttle_ms" => {
                if let Some(n) = v.as_u64() {
                    self.throttle_ms = n as u32;
                }
            }
            "miss_enable" => {
                if let Some(b) = v.as_bool() {
                    self.miss.enable = b;
                }
            }
            "miss_strength" => {
                if let Some(n) = v.as_i64() {
                    self.miss.delta = n as i32;
                }
            }
            "miss_duration" => {
                if let Some(n) = v.as_f64() {
                    self.miss.duration = n;
                }
            }
            "miss_preset" => {
                if let Some(s) = v.as_str() {
                    self.miss.preset = s.to_owned();
                }
            }
            "bad_enable" => {
                if let Some(b) = v.as_bool() {
                    self.bad.enable = b;
                }
            }
            "bad_strength" => {
                if let Some(n) = v.as_i64() {
                    self.bad.delta = n as i32;
                }
            }
            "bad_duration" => {
                if let Some(n) = v.as_f64() {
                    self.bad.duration = n;
                }
            }
            "bad_preset" => {
                if let Some(s) = v.as_str() {
                    self.bad.preset = s.to_owned();
                }
            }
            "good_enable" => {
                if let Some(b) = v.as_bool() {
                    self.good.enable = b;
                }
            }
            "good_strength" => {
                if let Some(n) = v.as_i64() {
                    self.good.delta = n as i32;
                }
            }
            "perfect_enable" => {
                if let Some(b) = v.as_bool() {
                    self.perfect.enable = b;
                }
            }
            "perfect_strength" => {
                if let Some(n) = v.as_i64() {
                    self.perfect.delta = n as i32;
                }
            }
            _ => {
                debug!("dghub: unknown config key: {key}");
            }
        }
    }

    fn trigger(&self, g: Grade) -> Option<Value> {
        let cfg = self.cfg(g);
        if !cfg.enable {
            return None;
        }
        let action = if cfg.preset.is_empty() { "strength" } else { "both" };
        let mut msg = json!({
            "op": "trigger",
            "action": action,
            "delta_pct": cfg.delta,
            "strength_mode": "rollback",
            "duration_s": cfg.duration,
            "channel": if self.channel.is_empty() { "both" } else { self.channel.as_str() },
            "label": g.label(),
        });
        if action == "both" {
            msg["preset"] = Value::String(cfg.preset.clone());
        }
        Some(msg)
    }
}

/// 游戏线程持有的句柄：提供共享 sender + 状态供 build_update_fn 读取。
pub struct DghubHandle {
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<Grade>>>>,
    status: Arc<AtomicU8>,
}

impl DghubHandle {
    /// 发送一条判定（若后台任务存活）。
    fn send(&self, g: Grade) -> bool {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            tx.send(g).is_ok()
        } else {
            false
        }
    }

    /// 是否已成功握手。
    pub fn connected(&self) -> bool {
        self.status.load(Ordering::Relaxed) == STATUS_CONNECTED
    }
}

/// 启动后台 DGHub 会话任务，返回游戏线程用的句柄。
///
/// 必须在 tokio runtime 上下文中调用（phira 启动时已 `rt.enter()`）。
/// 把连接事件推入 [`drain_events`] 全局队列。
pub fn spawn(host: String, port: u16, token: Option<String>) -> DghubHandle {
    let tx_shared: Arc<Mutex<Option<mpsc::UnboundedSender<Grade>>>> = Arc::new(Mutex::new(None));
    let status: Arc<AtomicU8> = Arc::new(AtomicU8::new(STATUS_CONNECTING));
    DGHUB_CONNECTION_STATUS.store(STATUS_CONNECTING, Ordering::Relaxed);

    let (tx, rx) = mpsc::unbounded_channel();
    *tx_shared.lock().unwrap() = Some(tx);

    // 保存全局会话（含共享 sender）
    *DGHUB_SESSION.lock().unwrap() = Some(DghubSession {
        host: host.clone(),
        port,
        token: token.clone(),
        tx: Arc::clone(&tx_shared),
    });
    info!("dghub: spawn -> {host}:{port}");

    let mapping = Mapping::from_config(&crate::get_data().config);
    let s = Arc::clone(&status);
    tokio::spawn(async move {
        match run(host, port, token, mapping, rx, &s).await {
            Ok(()) => info!("dghub: session ended"),
            Err(err) => warn!("dghub: session error: {err:?}"),
        }
        if s.load(Ordering::Relaxed) == STATUS_CONNECTED {
            DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Disconnected("连接断开".into()));
        }
        s.store(0, Ordering::Relaxed);
        DGHUB_CONNECTION_STATUS.store(0, Ordering::Relaxed);
    });
    DghubHandle { tx: tx_shared, status }
}

async fn fetch_token(host: &str, port: u16) -> Result<String> {
    let url = format!("http://{host}:{port}/api/plugins/_session_token");
    debug!("dghub: fetch token from {url}");
    let text = reqwest::get(&url).await?.error_for_status()?.text().await?;
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if let Some(t) = v.get("token").and_then(Value::as_str) {
            info!("dghub: token fetched");
            return Ok(t.to_owned());
        }
        if let Some(t) = v.as_str() {
            info!("dghub: token fetched (raw string)");
            return Ok(t.to_owned());
        }
    }
    let t = text.trim().trim_matches('"').to_owned();
    info!("dghub: token fetched (plain)");
    Ok(t)
}

fn hello_message(token: &str) -> Value {
    json!({
        "op": "hello",
        "token": token,
        "manifest": {
            "id": "phira",
            "name": "Phira 判定联动",
            "version": env!("CARGO_PKG_VERSION"),
            "sdk": "1",
            "author": "phira",
            "description": "把 Phira 的 Perfect/Good/Bad/Miss 判定映射为电击触发。",
            "config_schema": [
                { "section": "通用", "fields": [
                    { "key": "channel", "type": "channel", "label": "输出通道", "default": "both" },
                    { "key": "throttle_ms", "type": "number", "label": "节流(ms)", "default": 80, "min": 0, "max": 1000, "description": "同一判定档在该间隔内多次命中只触发一次" }
                ]},
                { "section": "Miss", "fields": [
                    { "key": "miss_enable", "type": "bool", "label": "Miss 触发", "default": true },
                    { "key": "miss_strength", "type": "percent", "label": "Miss 强度", "default": 60 },
                    { "key": "miss_duration", "type": "duration", "label": "Miss 时长", "default": 1.5 },
                    { "key": "miss_preset", "type": "preset", "label": "Miss 波形", "default": "CS2-受伤" }
                ]},
                { "section": "Bad", "fields": [
                    { "key": "bad_enable", "type": "bool", "label": "Bad 触发", "default": true },
                    { "key": "bad_strength", "type": "percent", "label": "Bad 强度", "default": 35 },
                    { "key": "bad_duration", "type": "duration", "label": "Bad 时长", "default": 1.0 },
                    { "key": "bad_preset", "type": "preset", "label": "Bad 波形", "default": "CS2-受伤" }
                ]},
                { "section": "Perfect / Good", "fields": [
                    { "key": "good_enable", "type": "bool", "label": "Good 触发", "default": false },
                    { "key": "good_strength", "type": "percent", "label": "Good 强度", "default": 20 },
                    { "key": "perfect_enable", "type": "bool", "label": "Perfect 触发", "default": false },
                    { "key": "perfect_strength", "type": "percent", "label": "Perfect 强度", "default": 10 }
                ]}
            ]
        }
    })
}

async fn run(
    host: String,
    port: u16,
    token_override: Option<String>,
    mapping: Mapping,
    mut rx: mpsc::UnboundedReceiver<Grade>,
    status: &AtomicU8,
) -> Result<()> {
    let token = match token_override.filter(|s| !s.is_empty()) {
        Some(t) => {
            info!("dghub: using manual token");
            t
        }
        None => fetch_token(&host, port).await?,
    };
    let url = format!("ws://{host}:{port}/ws/plugin?token={token}");
    debug!("dghub: connecting to {url}");
    let (mut ws, _) = connect_async(url.as_str()).await?;
    info!("dghub: ws connected");

    let hello = hello_message(&token);
    debug!("dghub: sending hello");
    ws.send(Message::Text(hello.to_string())).await?;

    // 等握手结果。
    loop {
        let msg = ws.next().await.ok_or_else(|| anyhow!("connection closed before hello_ack"))??;
        if let Message::Text(text) = &msg {
            let v: Value = serde_json::from_str(text)?;
            if v.get("op").and_then(Value::as_str) == Some("hello_ack") {
                if v.get("accepted").and_then(Value::as_bool) != Some(true) {
                    let reason = v.get("reason").and_then(Value::as_str).unwrap_or("unknown");
                    warn!("dghub: hello rejected: {reason}");
                    return Err(anyhow!("hello rejected: {reason}"));
                }
                debug!("dghub: hello_ack received");
                break;
            }
        }
    }
    info!("dghub: handshake accepted, ready");
    status.store(STATUS_CONNECTED, Ordering::Relaxed);
    DGHUB_CONNECTION_STATUS.store(STATUS_CONNECTED, Ordering::Relaxed);
    DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Connected);

    let use_phira_config = crate::get_data().config.dghub_use_phira_config;
    if use_phira_config {
        info!("dghub: using phira-side config (ignoring DGHub config page)");
    }
    let mut mapping = mapping;
    let mut last_sent: [Option<Instant>; 4] = [None; 4];

    loop {
        tokio::select! {
            incoming = ws.next() => {
                let msg = match incoming {
                    Some(Ok(m)) => m,
                    Some(Err(err)) => return Err(err.into()),
                    None => return Ok(()), // 服务端断开
                };
                match msg {
                    Message::Text(text) => {
                        let v: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(e) => { debug!("dghub: bad json: {e}"); continue; }
                        };
                        match v.get("op").and_then(Value::as_str) {
                            Some("config") => {
                                if !use_phira_config {
                                    if let Some(data) = v.get("data") {
                                        if let Some(obj) = data.as_object() {
                                            for (k, val) in obj {
                                                mapping.apply_one(k, val);
                                            }
                                        }
                                    }
                                }
                            }
                            Some("config_changed") => {
                                if !use_phira_config {
                                    if let (Some(k), Some(val)) = (v.get("key").and_then(Value::as_str), v.get("value")) {
                                        mapping.apply_one(k, val);
                                    }
                                }
                            }
                            Some("ping") => {
                                let mut pong = json!({ "op": "pong" });
                                if let Some(t) = v.get("t") {
                                    pong["t"] = t.clone();
                                }
                                ws.send(Message::Text(pong.to_string())).await?;
                            }
                            Some("stop") => {
                                info!("dghub: stop requested by host");
                                return Ok(());
                            }
                            Some(other) => { debug!("dghub: server op: {other}"); }
                            None => {}
                        }
                    }
                    Message::Close(cf) => {
                        info!("dghub: ws closed by server: {cf:?}");
                        return Ok(());
                    }
                    _ => {}
                }
            }
            grade = rx.recv() => {
                let grade = match grade {
                    Some(g) => g,
                    None => return Ok(()), // 游戏线程 drop 了句柄 → 退出游戏
                };
                let idx = grade as usize;
                let throttle = Duration::from_millis(mapping.throttle_ms as u64);
                let now = Instant::now();
                if !throttle.is_zero() {
                    if let Some(last) = last_sent[idx] {
                        if now.duration_since(last) < throttle {
                            continue;
                        }
                    }
                }
                if let Some(msg) = mapping.trigger(grade) {
                    last_sent[idx] = Some(now);
                    debug!("dghub: trigger {grade:?} delta={} dur={}s preset={} ch={}",
                        msg.get("delta_pct").and_then(|v| v.as_i64()).unwrap_or(0),
                        msg.get("duration_s").and_then(|v| v.as_f64()).unwrap_or(0.),
                        msg.get("preset").and_then(|v| v.as_str()).unwrap_or("-"),
                        msg.get("channel").and_then(|v| v.as_str()).unwrap_or("both"));
                    ws.send(Message::Text(msg.to_string())).await?;
                }
            }
        }
    }
}

/// 构造游戏线程每帧调用的 `UpdateFn`：对 `judge.counts()` 做 diff，把新增的
/// 各档判定逐个推入 channel。不 drain `judgements`，与多人 live 共存。
/// 同时也会消费 [`drain_events`] 并调用回调。
pub fn build_update_fn(handle: DghubHandle, on_event: impl Fn(DghubEvent) + 'static) -> UpdateFn {
    let mut last = [0u32; 4];
    Box::new(move |_t, _res, judge| {
        for ev in drain_events() {
            on_event(ev);
        }
        let counts = judge.counts();
        for i in 0..4 {
            let mut n = counts[i].saturating_sub(last[i]);
            while n > 0 {
                if !handle.send(Grade::from_index(i)) {
                    return;
                }
                n -= 1;
            }
        }
        last = counts;
    })
}

/// 把两个可选 `UpdateFn` 串成一个；两个都 `None` 则返回 `None`。
/// 用于让 DGHub 的 update_fn 与多人 live 的 update_fn 共存。
pub fn chain_update_fns(a: Option<UpdateFn>, b: Option<UpdateFn>) -> Option<UpdateFn> {
    match (a, b) {
        (None, None) => None,
        (Some(f), None) | (None, Some(f)) => Some(f),
        (Some(mut f), Some(mut g)) => Some(Box::new(move |t, res, judge| {
            f(t, res, judge);
            g(t, res, judge);
        })),
    }
}
