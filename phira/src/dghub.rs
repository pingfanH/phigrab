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
use tracing::{info, warn};

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

static DGHUB_EVENTS: std::sync::LazyLock<Mutex<Vec<DghubEvent>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// 全局共享的 grade 发送端 + 重连参数。
/// `spawn` 写入，`build_update_fn` 和 `request_reconnect` 读取/替换。
static DGHUB_SESSION: std::sync::LazyLock<Mutex<Option<DghubSession>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

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

/// 尝试重连：复用共享 sender（若有），起新后台任务。
pub fn request_reconnect() {
    if let Some(session) = DGHUB_SESSION.lock().unwrap().as_ref() {
        let host = session.host.clone();
        let port = session.port;
        let token = session.token.clone();
        let tx_shared = Arc::clone(&session.tx);

        // 为新任务建一对新 channel；替换共享 sender
        let (new_tx, rx) = mpsc::unbounded_channel();
        *tx_shared.lock().unwrap() = Some(new_tx);

        let status: Arc<AtomicU8> = Arc::new(AtomicU8::new(STATUS_CONNECTING));
        tokio::spawn(async move {
            match run(host, port, token, rx, &status).await {
                Ok(()) => info!("dghub: session ended"),
                Err(err) => warn!("dghub: session error: {err:?}"),
            }
            if status.load(Ordering::Relaxed) == STATUS_CONNECTED {
                DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Disconnected("连接断开".into()));
            }
        });
    }
}

// ---------------------------------------------------------------------------
// 映射 / GradeCfg
// ---------------------------------------------------------------------------

/// 单档判定的触发参数。
#[derive(Clone)]
struct GradeCfg {
    enable: bool,
    delta: i32,
    duration: f64,
    preset: String,
}

/// 判定 → trigger 的完整映射；默认值见 [`Default`]（「只惩罚失误」）。
/// 全量由 DGHub 配置页驱动，phira 收 `config` / `config_changed` 后更新。
#[derive(Clone)]
struct Mapping {
    channel: String,
    throttle_ms: u64,
    perfect: GradeCfg,
    good: GradeCfg,
    bad: GradeCfg,
    miss: GradeCfg,
}

impl Default for Mapping {
    fn default() -> Self {
        Self {
            channel: "both".to_owned(),
            throttle_ms: 80,
            perfect: GradeCfg { enable: false, delta: 10, duration: 1.0, preset: String::new() },
            good: GradeCfg { enable: false, delta: 20, duration: 1.0, preset: String::new() },
            bad: GradeCfg { enable: true, delta: 35, duration: 1.0, preset: "CS2-受伤".to_owned() },
            miss: GradeCfg { enable: true, delta: 60, duration: 1.5, preset: "CS2-受伤".to_owned() },
        }
    }
}

impl Mapping {
    fn cfg(&self, g: Grade) -> &GradeCfg {
        match g {
            Grade::Perfect => &self.perfect,
            Grade::Good => &self.good,
            Grade::Bad => &self.bad,
            Grade::Miss => &self.miss,
        }
    }

    /// 应用一条 `config_changed`（或 `config` 全量里的一项）。
    ///
    /// DGHub 的 `config_schema` 不给 field 显式 `key`，而是由主程序根据 `label`
    /// 自动生成 key（规则：label 去空格/标点、全小写、可能前缀 section 序号）。
    /// 因此这里不硬编码 key 名，而是检测 key 中包含的语义片段来匹配。
    fn apply_one(&mut self, key: &str, v: &Value) {
        let key_lower = key.to_lowercase();

        // 通用字段
        if key_lower.contains("channel") || key_lower.contains("通道") {
            if let Some(s) = v.as_str() {
                self.channel = s.to_owned();
            }
            return;
        }
        if key_lower.contains("throttle") || key_lower.contains("节流") {
            if let Some(n) = v.as_u64() {
                self.throttle_ms = n;
            }
            return;
        }

        // 判定档匹配：key 里同时包含档名 + 属性语义
        for (grade, cfg) in [
            (Grade::Perfect, &mut self.perfect),
            (Grade::Good, &mut self.good),
            (Grade::Bad, &mut self.bad),
            (Grade::Miss, &mut self.miss),
        ] {
            let gname = grade.label().to_lowercase(); // "perfect"/"good"/"bad"/"miss"
            if !key_lower.contains(&gname) {
                continue;
            }
            // 属性语义
            if key_lower.contains("enable") || key_lower.contains("触发") {
                if let Some(b) = v.as_bool() {
                    cfg.enable = b;
                }
            } else if key_lower.contains("delta") || key_lower.contains("强度") {
                if let Some(n) = v.as_i64() {
                    cfg.delta = n as i32;
                }
            } else if key_lower.contains("duration") || key_lower.contains("时长") {
                if let Some(n) = v.as_f64() {
                    cfg.duration = n;
                }
            } else if key_lower.contains("preset") || key_lower.contains("波形") {
                if let Some(s) = v.as_str() {
                    cfg.preset = s.to_owned();
                }
            }
            return;
        }
    }

    /// 应用一份 `config` 全量数据对象。
    fn apply_full(&mut self, data: &Value) {
        if let Some(obj) = data.as_object() {
            for (k, v) in obj {
                self.apply_one(k, v);
            }
        }
    }

    /// 为某判定组一条 `trigger` 消息；不启用则返回 `None`。
    fn trigger(&self, g: Grade) -> Option<Value> {
        let cfg = self.cfg(g);
        if !cfg.enable {
            return None;
        }
        // 有预设 → 同时改强度 + 播波形；无预设 → 仅强度层。
        let action = if cfg.preset.is_empty() { "strength" } else { "both" };
        let mut msg = json!({
            "op": "trigger",
            "action": action,
            "delta_pct": cfg.delta,
            "strength_mode": "rollback",
            "duration_s": cfg.duration,
            "channel": self.channel,
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

    let (tx, rx) = mpsc::unbounded_channel();
    *tx_shared.lock().unwrap() = Some(tx);

    // 保存全局会话（含共享 sender）
    *DGHUB_SESSION.lock().unwrap() = Some(DghubSession {
        host: host.clone(),
        port,
        token: token.clone(),
        tx: Arc::clone(&tx_shared),
    });

    let s = Arc::clone(&status);
    tokio::spawn(async move {
        match run(host, port, token, rx, &s).await {
            Ok(()) => info!("dghub: session ended"),
            Err(err) => warn!("dghub: session error: {err:?}"),
        }
        if s.load(Ordering::Relaxed) == STATUS_CONNECTED {
            DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Disconnected("连接断开".into()));
        }
        s.store(0, Ordering::Relaxed);
    });
    DghubHandle { tx: tx_shared, status }
}

async fn fetch_token(host: &str, port: u16) -> Result<String> {
    let url = format!("http://{host}:{port}/api/plugins/_session_token");
    let text = reqwest::get(&url).await?.error_for_status()?.text().await?;
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if let Some(t) = v.get("token").and_then(Value::as_str) {
            return Ok(t.to_owned());
        }
        if let Some(t) = v.as_str() {
            return Ok(t.to_owned());
        }
    }
    Ok(text.trim().trim_matches('"').to_owned())
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
            "config_schema": config_schema(),
        }
    })
}

fn config_schema() -> Value {
    json!([
        { "fields": [
            { "type": "channel", "label": "输出通道", "default": "both" },
            { "type": "number", "label": "节流(ms)", "default": 80, "min": 0, "max": 1000,
              "description": "同一判定档在该间隔内多次命中只触发一次" }
        ]},
        { "fields": [
            { "type": "bool", "label": "Miss 触发", "default": true },
            { "type": "percent", "label": "Miss 强度", "default": 60 },
            { "type": "duration", "label": "Miss 时长", "default": 1.5 },
            { "type": "preset", "label": "Miss 波形", "default": "CS2-受伤" }
        ]},
        { "fields": [
            { "type": "bool", "label": "Bad 触发", "default": true },
            { "type": "percent", "label": "Bad 强度", "default": 35 },
            { "type": "duration", "label": "Bad 时长", "default": 1.0 },
            { "type": "preset", "label": "Bad 波形", "default": "CS2-受伤" }
        ]},
        { "fields": [
            { "type": "bool", "label": "Good 触发", "default": false },
            { "type": "percent", "label": "Good 强度", "default": 20 },
            { "type": "bool", "label": "Perfect 触发", "default": false },
            { "type": "percent", "label": "Perfect 强度", "default": 10 }
        ]}
    ])
}

async fn run(
    host: String, port: u16, token_override: Option<String>,
    mut rx: mpsc::UnboundedReceiver<Grade>,
    status: &AtomicU8,
) -> Result<()> {
    let token = match token_override.filter(|s| !s.is_empty()) {
        Some(t) => t,
        None => fetch_token(&host, port).await?,
    };
    let url = format!("ws://{host}:{port}/ws/plugin?token={token}");
    let (mut ws, _) = connect_async(url.as_str()).await?;
    info!("dghub: connected to {url}");

    ws.send(Message::Text(hello_message(&token).to_string())).await?;

    // 等握手结果。
    loop {
        let msg = ws.next().await.ok_or_else(|| anyhow!("connection closed before hello_ack"))??;
        if let Message::Text(text) = &msg {
            let v: Value = serde_json::from_str(text)?;
            if v.get("op").and_then(Value::as_str) == Some("hello_ack") {
                if v.get("accepted").and_then(Value::as_bool) != Some(true) {
                    let reason = v.get("reason").and_then(Value::as_str).unwrap_or("unknown");
                    return Err(anyhow!("hello rejected: {reason}"));
                }
                break;
            }
        }
    }
    info!("dghub: handshake accepted");
    status.store(STATUS_CONNECTED, Ordering::Relaxed);
    DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Connected);

    let mut mapping = Mapping::default();
    // 各档上次发送时刻，用于节流。
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
                            Err(_) => continue,
                        };
                        match v.get("op").and_then(Value::as_str) {
                            Some("config") => {
                                if let Some(data) = v.get("data") {
                                    mapping.apply_full(data);
                                }
                            }
                            Some("config_changed") => {
                                if let (Some(k), Some(val)) = (v.get("key").and_then(Value::as_str), v.get("value")) {
                                    mapping.apply_one(k, val);
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
                            _ => {}
                        }
                    }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
            grade = rx.recv() => {
                let grade = match grade {
                    Some(g) => g,
                    None => return Ok(()), // 游戏线程 drop 了句柄 → 退出游戏
                };
                let idx = grade as usize;
                let throttle = Duration::from_millis(mapping.throttle_ms);
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
