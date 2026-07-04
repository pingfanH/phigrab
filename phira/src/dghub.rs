//! DGHub 判定联动 —— phira 进程内直接实现 DGHub 插件协议 (SDK v1)。
//!
//! 设计文档见 `dghub/PHIRA_INTEGRATION.md`。
//!
//! ## 连接状态反馈
//!
//! 后台任务把连接/断开事件写入全局队列 [`drain_events`]。
//! 主循环每帧轮询，发现事件后通过 `show_message` 弹窗告知用户。

use anyhow::{anyhow, Result};
use futures_util::{stream::FuturesUnordered, SinkExt, StreamExt};
use prpr::scene::UpdateFn;
use reqwest::Client;
use serde_json::{json, Value};
use std::{
    net::{IpAddr, Ipv4Addr, UdpSocket},
    sync::{atomic::AtomicU8, atomic::Ordering, Arc, Mutex},
};
use tokio::{
    net::TcpStream,
    sync::mpsc,
    time::{timeout, Duration, Instant},
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
    Scanning,
    Connected { host: String, port: u16 },
    Disconnected(String),
    ScanFailed,
}

/// 连接状态。
const STATUS_CONNECTING: u8 = 1;
const STATUS_CONNECTED: u8 = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

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

fn handle_from_session() -> Option<DghubHandle> {
    if connection_status() == 0 {
        return None;
    }
    DGHUB_SESSION
        .lock()
        .unwrap()
        .as_ref()
        .map(|session| DghubHandle { tx: Arc::clone(&session.tx) })
}

fn update_session_endpoint(host: &str, port: u16) {
    if let Some(session) = DGHUB_SESSION.lock().unwrap().as_mut() {
        session.host = host.to_owned();
        session.port = port;
    }
}

/// 尝试重连：复用共享 sender（若有），起新后台任务。
pub fn request_reconnect() {
    let (enabled, host, port, token) = {
        let cfg = &crate::get_data().config;
        (cfg.dghub_enable, cfg.dghub_host.clone(), cfg.dghub_port, normalize_token(&cfg.dghub_token))
    };

    if !enabled {
        warn!("dghub: reconnect requested but DGHub is disabled");
        return;
    }

    let tx_shared = {
        let mut session = DGHUB_SESSION.lock().unwrap();
        if let Some(session) = session.as_mut() {
            session.host = host.clone();
            session.port = port;
            session.token = token.clone();
            Some(Arc::clone(&session.tx))
        } else {
            None
        }
    };

    if let Some(tx_shared) = tx_shared {
        info!("dghub: reconnect requested -> {host}:{port}");

        // 为新任务建一对新 channel；替换共享 sender
        let (new_tx, rx) = mpsc::unbounded_channel();
        *tx_shared.lock().unwrap() = Some(new_tx);

        let mapping = Mapping::from_config(&crate::get_data().config);
        let status: Arc<AtomicU8> = Arc::new(AtomicU8::new(STATUS_CONNECTING));
        DGHUB_CONNECTION_STATUS.store(STATUS_CONNECTING, Ordering::Relaxed);
        tokio::spawn(async move {
            let mut rx = rx;
            run_with_optional_scan(host, port, token, mapping, &mut rx, &status, true).await;
        });
    } else {
        info!("dghub: reconnect requested without session; spawning from config -> {host}:{port}");
        let _ = spawn_with_scan(host, port, token, true);
    }
}

pub fn normalize_token(token: &str) -> Option<String> {
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

pub fn start_from_config() -> Option<DghubHandle> {
    if let Some(handle) = handle_from_session() {
        return Some(handle);
    }

    let cfg = &crate::get_data().config;
    if !cfg.dghub_enable {
        return None;
    }
    Some(spawn_with_scan(cfg.dghub_host.clone(), cfg.dghub_port, normalize_token(&cfg.dghub_token), true))
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
            "good_duration" => {
                if let Some(n) = v.as_f64() {
                    self.good.duration = n;
                }
            }
            "good_preset" => {
                if let Some(s) = v.as_str() {
                    self.good.preset = s.to_owned();
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
            "perfect_duration" => {
                if let Some(n) = v.as_f64() {
                    self.perfect.duration = n;
                }
            }
            "perfect_preset" => {
                if let Some(s) = v.as_str() {
                    self.perfect.preset = s.to_owned();
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

/// 游戏线程持有的句柄：提供共享 sender 供 build_update_fn 发送判定。
pub struct DghubHandle {
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<Grade>>>>,
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
}

/// 启动后台 DGHub 会话任务，返回游戏线程用的句柄。
///
/// 必须在 tokio runtime 上下文中调用（phira 启动时已 `rt.enter()`）。
/// 把连接事件推入 [`drain_events`] 全局队列。
fn spawn_with_scan(host: String, port: u16, token: Option<String>, scan_on_failure: bool) -> DghubHandle {
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
        let mut rx = rx;
        run_with_optional_scan(host, port, token, mapping, &mut rx, &s, scan_on_failure).await;
    });
    DghubHandle { tx: tx_shared }
}

async fn run_with_optional_scan(
    host: String,
    port: u16,
    token: Option<String>,
    mapping: Mapping,
    rx: &mut mpsc::UnboundedReceiver<Grade>,
    status: &AtomicU8,
    scan_on_failure: bool,
) {
    let mut ever_connected = false;
    match run_retry_latest_token(host.clone(), port, token.clone(), mapping.clone(), rx, status).await {
        Ok(()) => info!("dghub: session ended"),
        Err(err) => {
            let was_connected = status.load(Ordering::Relaxed) == STATUS_CONNECTED;
            ever_connected |= was_connected;
            warn!("dghub: session error: {err:?}");
            if scan_on_failure && !was_connected {
                DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Scanning);
                if let Some((scan_host, scan_port)) = scan_lan().await {
                    info!("dghub: scan found -> {scan_host}:{scan_port}");
                    update_session_endpoint(&scan_host, scan_port);
                    let mapping = Mapping::from_config(&crate::get_data().config);
                    match run_retry_latest_token(scan_host, scan_port, token.clone(), mapping, rx, status).await {
                        Ok(()) => info!("dghub: scanned session ended"),
                        Err(err) => warn!("dghub: scanned session error: {err:?}"),
                    }
                    let scan_connected = status.load(Ordering::Relaxed) == STATUS_CONNECTED;
                    ever_connected |= scan_connected;
                    if !scan_connected {
                        DGHUB_EVENTS.lock().unwrap().push(DghubEvent::ScanFailed);
                    }
                } else {
                    warn!("dghub: scan failed");
                    DGHUB_EVENTS.lock().unwrap().push(DghubEvent::ScanFailed);
                }
            }
        }
    }
    if ever_connected || status.load(Ordering::Relaxed) == STATUS_CONNECTED {
        DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Disconnected("连接断开".into()));
    }
    status.store(0, Ordering::Relaxed);
    DGHUB_CONNECTION_STATUS.store(0, Ordering::Relaxed);
}

async fn run_retry_latest_token(
    host: String,
    port: u16,
    token: Option<String>,
    mapping: Mapping,
    rx: &mut mpsc::UnboundedReceiver<Grade>,
    status: &AtomicU8,
) -> Result<()> {
    let has_manual_token = token.is_some();
    match run(host.clone(), port, token, mapping.clone(), rx, status).await {
        Err(err) if has_manual_token && status.load(Ordering::Relaxed) != STATUS_CONNECTED => {
            warn!("dghub: manual token failed before handshake: {err:?}; retrying with latest token");
            run(host, port, None, mapping, rx, status).await
        }
        result => result,
    }
}

fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

async fn probe_endpoint(client: Client, host: String, port: u16) -> Option<(String, u16)> {
    let url = format!("http://{host}:{port}/api/plugins/_session_token");
    client.get(url).send().await.ok()?.error_for_status().ok()?;
    Some((host, port))
}

async fn probe_device_port(host: String, port: u16) -> bool {
    timeout(Duration::from_millis(120), TcpStream::connect((host.as_str(), port)))
        .await
        .is_ok_and(|res| res.is_ok())
}

async fn probe_device(host: String) -> Option<String> {
    const DEVICE_PROBE_PORTS: [u16; 2] = [80, 443];

    let mut probes = FuturesUnordered::new();
    for port in DEVICE_PROBE_PORTS {
        probes.push(probe_device_port(host.clone(), port));
    }
    while let Some(found) = probes.next().await {
        if found {
            return Some(host);
        }
    }
    None
}

fn current_segment_hosts() -> Vec<String> {
    let mut hosts = vec!["127.0.0.1".to_owned()];
    let Some(local) = local_ipv4() else {
        return hosts;
    };
    let octets = local.octets();

    for it in 1u8..=254 {
        let host = format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], it);
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

async fn scan_lan_devices(hosts: &[String]) -> Vec<String> {
    const CONCURRENCY: usize = 32;

    let mut hosts = hosts.iter().cloned();
    let mut devices = Vec::new();
    loop {
        let mut probes = FuturesUnordered::new();
        for host in hosts.by_ref().take(CONCURRENCY) {
            probes.push(probe_device(host));
        }
        if probes.is_empty() {
            break;
        }
        while let Some(found) = probes.next().await {
            if let Some(host) = found {
                devices.push(host);
            }
        }
    }
    devices
}

fn log_ip_list(label: &str, ips: &[String]) {
    if ips.is_empty() {
        info!("dghub: {label}: none");
        return;
    }
    for chunk in ips.chunks(32) {
        info!("dghub: {label}: {}", chunk.join(", "));
    }
}

async fn scan_lan() -> Option<(String, u16)> {
    const START_PORT: u16 = 8000;
    const END_PORT: u16 = 8003;
    const CONCURRENCY: usize = 64;

    let candidates = current_segment_hosts();
    if candidates.is_empty() {
        warn!("dghub: lan scan failed to resolve current segment");
        return None;
    }
    let devices = scan_lan_devices(&candidates).await;
    let device_count = devices.len();
    log_ip_list("detected responsive ips", &devices);
    let mut scan_hosts = devices;
    for host in candidates {
        if !scan_hosts.contains(&host) {
            scan_hosts.push(host);
        }
    }
    log_ip_list("endpoint scan ips", &scan_hosts);
    info!("dghub: lan scan found {device_count} responsive device(s), scanning {} host(s)", scan_hosts.len());

    let client = Client::builder().timeout(Duration::from_millis(350)).build().ok()?;
    for port in START_PORT..=END_PORT {
        let mut hosts = scan_hosts.iter().cloned();
        loop {
            let mut probes = FuturesUnordered::new();
            for host in hosts.by_ref().take(CONCURRENCY) {
                probes.push(probe_endpoint(client.clone(), host, port));
            }
            if probes.is_empty() {
                break;
            }
            while let Some(found) = probes.next().await {
                if found.is_some() {
                    return found;
                }
            }
        }
    }
    None
}

async fn fetch_token(host: &str, port: u16) -> Result<String> {
    let url = format!("http://{host}:{port}/api/plugins/_session_token");
    debug!("dghub: fetch token from {url}");
    let client = Client::builder().timeout(CONNECT_TIMEOUT).build()?;
    let text = client.get(&url).send().await?.error_for_status()?.text().await?;
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
                    { "key": "github_url", "type": "text", "label": "GitHub 链接", "default": "https://github.com/pingfanH/phigrab", "description": "仅展示，可复制" },
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
                    { "key": "good_duration", "type": "duration", "label": "Good 时长", "default": 1.0 },
                    { "key": "good_preset", "type": "preset", "label": "Good 波形", "default": "" },
                    { "key": "perfect_enable", "type": "bool", "label": "Perfect 触发", "default": false },
                    { "key": "perfect_strength", "type": "percent", "label": "Perfect 强度", "default": 10 },
                    { "key": "perfect_duration", "type": "duration", "label": "Perfect 时长", "default": 1.0 },
                    { "key": "perfect_preset", "type": "preset", "label": "Perfect 波形", "default": "" }
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
    rx: &mut mpsc::UnboundedReceiver<Grade>,
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
    let (mut ws, _) = timeout(CONNECT_TIMEOUT, connect_async(url.as_str())).await??;
    info!("dghub: ws connected");

    let hello = hello_message(&token);
    debug!("dghub: sending hello");
    ws.send(Message::Text(hello.to_string())).await?;

    // 等握手结果。
    loop {
        let msg = timeout(CONNECT_TIMEOUT, ws.next())
            .await?
            .ok_or_else(|| anyhow!("connection closed before hello_ack"))??;
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
    DGHUB_EVENTS.lock().unwrap().push(DghubEvent::Connected { host: host.clone(), port });

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
pub fn build_update_fn(handle: DghubHandle) -> UpdateFn {
    let mut last = [0u32; 4];
    Box::new(move |_t, _res, judge| {
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
