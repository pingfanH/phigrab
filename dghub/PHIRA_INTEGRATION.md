# Phira × DGHub 判定联动 — 设计文档

> 目标：phira 在游戏过程中产生的判定（Perfect / Good / Bad / Miss）实时驱动
> DGHub 的电击波形与强度。采用 **Rust 原生直连** 架构 —— phira 进程内直接实现
> DGHub 插件协议（SDK v1），无需独立的 Python 桥接进程。

本文档先确定架构与协议映射，随后给出落地实现步骤。代码完成后本文档作为该
功能的权威说明同步更新。

---

## 1. 架构总览

```
phira (单进程, Rust)
  │
  ├─ prpr::judge::Judge.update()           每帧产生 (t, line_id, note_id, Judgement)
  │        └─ judge.judgements (RefCell)    判定队列，每帧被 update_fn 排空
  │
  ├─ UpdateFn 钩子 (loading.rs:25)          每帧在 game.rs:1058 被调用
  │        └─ dghub emitter 闭包            排空判定 → 发到内部 channel
  │
  └─ DghubClient (新增模块, tokio 任务)
           ├─ GET http://127.0.0.1:PORT/api/plugins/_session_token   拉 token
           ├─ ws://127.0.0.1:PORT/ws/plugin?token=...                握手 hello
           ├─ 收 config / config_changed                            更新映射表
           └─ 每条判定 → 按映射表组 trigger 消息 → 发送
                                  │
                                  ▼
                            DGHub 主程序  ── 自动生成配置 UI（来自 hello.manifest.config_schema）
```

要点：

- **DGHub 不 spawn phira**。phira 是用户主动启动的游戏，按文档 §4 的「手动接入」
  方式：自己拉 `/api/plugins/_session_token` 拿 token，再连 `ws/plugin`。
- DGHub 端的配置 UI 由 phira 在 `hello` 时上报的 `config_schema` 自动生成；
  用户在 DGHub 里改映射 → DGHub 推 `config_changed` → phira 实时更新映射表。
- phira 端只需一个开关 + host/port（连不连、连哪台），映射细节放 DGHub 配置页。
  （满足「可以在 phira 或 dghub 设置」：连接参数在 phira，映射参数在 dghub。）

---

## 2. 判定数据来源（已确认）

| 项 | 位置 |
|---|---|
| 判定枚举 `Judgement { Perfect, Good, Bad, Miss }` | `prpr/src/judge.rs:145-152`（`#[repr(u8)]`，已 `Serialize`） |
| 判定队列 `judgements: RefCell<Vec<(f64,u32,u32,Result<Judgement,bool>)>>` | `prpr/src/judge.rs:268,281` |
| 唯一提交点 `Judge::commit()` | `prpr/src/judge.rs:347-350` |
| 每帧调用 `judge.update()` | `prpr/src/scene/game.rs:1055` |
| **`update_fn` 钩子调用点** | `prpr/src/scene/game.rs:1058-1060` |
| `UpdateFn` 类型定义 | `prpr/src/scene/loading.rs:25` |
| 现有 `update_fn` 生产者（live 多人） | `phira/src/scene/song.rs:872-972` |

元组第 4 项 `Result<Judgement, bool>`：`Ok(j)` 是普通判定；`Err(true)`=HoldPerfect、
`Err(false)`=HoldGood（长按起手）。映射时把 Hold 起手并入对应 Perfect/Good 档，
或忽略（见 §4 默认）。

**关键约束**：`update_fn` 只有一个槽。song.rs 在 live 多人时已占用它。
新增逻辑必须 **与现有闭包组合（chain）**，不能覆盖。见 §6.2。

---

## 3. DGHub 协议要点（来自 PLUGIN_DEVELOPMENT.md）

- 握手：第一条必发 `hello`，带 `token` + `manifest`（含 `id/name/version/sdk:"1"` 与
  `config_schema`）。收 `hello_ack`，`accepted` 必须为 true。
- 触发：统一用 **`trigger`** 消息：
  ```jsonc
  { "op":"trigger", "action":"both", "delta_pct":50, "strength_mode":"rollback",
    "duration_s":1.5, "preset":"CS2-受伤", "channel":"both", "label":"Miss" }
  ```
  - `action`: `both`|`strength`|`waveform`
  - `delta_pct`: -100~100，相对 baseline 的增量
  - `strength_mode`: `rollback`（duration 后自动回正）|`permanent`
  - `duration_s`: 0~300
  - `preset`: 波形预设名（`action` 含 waveform 时必填）
  - `channel`: `a`|`b`|`both`
- 服务端推：`config`（握手后全量）、`config_changed`（用户改配置）、
  `device_info`、`stop`、`ping`/`pong`。
- **`extra="forbid"`**：发了协议没有的字段会被拒绝；字段缺失则取默认值。
- token：`GET /api/plugins/_session_token`（同机 127.0.0.1，无需鉴权即可拉）。

---

## 4. 判定 → trigger 映射（默认值，DGHub 配置页可改）

每个判定档独立配置一组 trigger 参数。默认采用「只惩罚失误」：

| 判定 | enable | action | delta_pct | duration_s | preset | strength_mode |
|---|---|---|---|---|---|---|
| Perfect | false | — | — | — | — | — |
| Good    | false | — | — | — | — | — |
| Bad     | true  | both | 35 | 1.0 | `CS2-受伤` | rollback |
| Miss    | true  | both | 60 | 1.5 | `CS2-受伤` | rollback |

- Hold 起手（`Err(true/false)`）默认 **忽略**（避免长按刷屏），并入 Perfect/Good
  档时也是 disable，所以默认不触发。
- `channel` 全局一个值（默认 `both`），不按档分。
- 节流：同一档在 `min_interval_ms`（默认 80ms）内多次命中只发一次，避免高密度
  段落把 DGHub 刷爆。Miss 不节流（漏的应该都疼一下）—— 此为可配置项。

这些字段全部进 `manifest.config_schema`，DGHub 自动渲染配置页。

---

## 5. manifest / hello 上报的 config_schema

phira 握手时上报（无独立 manifest.json 文件，直接在 Rust 里构造 JSON）：

```jsonc
{
  "op": "hello",
  "token": "<拉到的 token>",
  "manifest": {
    "id": "phira",
    "name": "Phira 判定联动",
    "version": "<phira 版本>",
    "sdk": "1",
    "author": "phira",
    "description": "把 Phira 的 Perfect/Good/Bad/Miss 判定映射为电击触发。",
    "config_schema": [
      { "section": "通用", "fields": [
        { "key": "channel", "type": "channel", "label": "输出通道", "default": "both" },
        { "key": "throttle_ms", "type": "number", "label": "节流(ms)", "default": 80, "min": 0, "max": 1000 }
      ]},
      { "section": "Bad", "fields": [
        { "key": "bad_enable",   "type": "bool",     "label": "Bad 触发",  "default": true },
        { "key": "bad_delta",    "type": "percent",  "label": "Bad 强度",  "default": 35 },
        { "key": "bad_duration", "type": "duration", "label": "Bad 时长",  "default": 1.0 },
        { "key": "bad_preset",   "type": "preset",   "label": "Bad 波形",  "default": "CS2-受伤" }
      ]},
      { "section": "Miss", "fields": [
        { "key": "miss_enable",   "type": "bool",     "label": "Miss 触发", "default": true },
        { "key": "miss_delta",    "type": "percent",  "label": "Miss 强度", "default": 60 },
        { "key": "miss_duration", "type": "duration", "label": "Miss 时长", "default": 1.5 },
        { "key": "miss_preset",   "type": "preset",   "label": "Miss 波形", "default": "CS2-受伤" }
      ]},
      { "section": "Perfect/Good", "fields": [
        { "key": "perfect_enable", "type": "bool", "label": "Perfect 触发", "default": false },
        { "key": "perfect_delta",  "type": "percent", "label": "Perfect 强度", "default": 10 },
        { "key": "good_enable",    "type": "bool", "label": "Good 触发", "default": false },
        { "key": "good_delta",     "type": "percent", "label": "Good 强度", "default": 20 }
      ]}
    ]
  }
}
```

phira 收到 `config`（全量）与 `config_changed`（增量）后，把这些 key 解析进内存
中的 `DghubMapping` 结构，下一条判定即生效。

---

## 6. 落地实现

### 6.1 依赖

`phira` crate 已有 `tokio`、`reqwest`、`serde_json`、`futures-util`。
需新增 WebSocket 客户端：

```toml
# phira/Cargo.toml [dependencies]
tokio-tungstenite = { version = "0.24", default-features = false, features = ["connector", "rustls-tls-webpki-roots"] }
```

（ws:// 明文连本地，无需 TLS feature，但保留以防将来。实际只用到 `connect_async`。
若想零 TLS 依赖可用 `default-features=false` 不带任何 tls feature。）

新增模块：`phira/src/dghub.rs`，并在 `phira/src/lib.rs` 注册 `mod dghub;`。

### 6.2 模块职责 `phira/src/dghub.rs`

- `pub struct DghubMapping`：每档的 enable/delta/duration/preset + 全局 channel/throttle。
  `Default` = §4 默认值。
- `pub enum Grade { Perfect, Good, Bad, Miss, HoldPerfect, HoldGood }`：从
  `prpr::judge::Judgement` + `Result` 转换。
- `pub struct DghubHandle`：持有一个 `tokio::sync::mpsc::Sender<Grade>`。游戏侧只往
  里塞判定，不阻塞。
- `pub fn spawn(host, port) -> DghubHandle`：在 tokio runtime 起后台任务：
  1. `reqwest::get(http://host:port/api/plugins/_session_token)` 拉 token；
  2. `connect_async(ws://host:port/ws/plugin?token=...)`；
  3. 发 `hello`，等 `hello_ack`；
  4. select! 循环：
     - 收 ws：`config`/`config_changed` → 更新 `Arc<Mutex<DghubMapping>>`；
       `ping` → 回 `pong`；`stop` → 退出。
     - 收 mpsc 判定：查映射，组 `trigger`，按节流发送。
  5. 断线/出错：`tracing::warn!` 后退出（不 panic）；下次进游戏重连。
- `pub fn build_update_fn(handle) -> UpdateFn`：返回一个闭包，每帧
  `judge.judgements.borrow()` 里 **不 drain**（song.rs 的 live 闭包会 drain），
  改为读 `Judge` 的提交计数差值 —— 见下方「与 live 共存」注意。

> ⚠️ **与 live 共存的取舍**：`judgements` 队列被谁 drain 谁清空。song.rs 的 live
> 闭包 `drain(..)`。若 dghub 闭包也 drain，两者抢同一队列会丢事件。
> 方案 A（推荐）：在 `Judge` 增量计数上做 diff —— 记录上帧 `counts:[u32;4]` 与
> combo，本帧比较得出新增的各档数量，不依赖 drain。这样与 live 完全解耦。
> 方案 B：在 song.rs 把判定 drain 出来后 fan-out 给 live 和 dghub 两个 sink。
> 本设计采用 **方案 A**：dghub emitter 只读 `judge.inner` 的计数，零侵入 prpr。

需在 `prpr/src/judge.rs` 暴露只读访问：每档累计数 `counts()` 与 combo。
检查 `JudgeInner` 是否已有 `pub fn counts()` / `combo`；若无则加只读 getter
（`prpr/src/judge.rs:154-260` 的 `JudgeInner`）。Hold 起手不进 `counts`，
方案 A 下默认不处理 Hold（与 §4 默认一致）。

### 6.3 接入点 `phira/src/scene/song.rs`

在构造 `update_fn`（song.rs:872）处，把 dghub 的闭包与现有 live 闭包 **组合**：

```rust
// 伪代码
let dghub_fn: Option<UpdateFn> = if get_data().config.dghub_enable {
    let handle = dghub::spawn(host, port);          // 读 config.dghub_host/port
    Some(dghub::build_update_fn(handle))
} else { None };

let update_fn = chain_update_fns(live_update_fn, dghub_fn);
```

`chain_update_fns(a, b)`：两个都 None → None；否则返回一个闭包依次调用存在的那个/两个。

### 6.4 phira 配置 `prpr/src/config.rs`

`Config`（config.rs:50-90，camelCase JSON）新增：

```rust
#[serde(default)] pub dghub_enable: bool,            // 默认 false
#[serde(default = "default_dghub_host")] pub dghub_host: String,  // "127.0.0.1"
#[serde(default = "default_dghub_port")] pub dghub_port: u16,     // DGHub API 端口，默认见下
```

DGHub API 端口默认值：文档未写死端口（运行时分配/可配），所以做成用户可填。
默认填一个占位（如 `8765`）并在设置页提示「与 DGHub 设置里的 API 端口一致」。
> 落地前需向 DGHub 确认默认端口；若 DGHub 有固定默认就用它。

`Default for Config`（config.rs:92-131）补三个字段默认值。

### 6.5 phira 设置 UI `phira/src/page/settings.rs`

在 `GeneralList`（或新开一档）加：
- `dghub_enable` 开关（`render_switch` 模式，参考 offline_btn，settings.rs:604-607）
- `dghub_host` 文本输入（参考 mp_addr_btn `request_input`，settings.rs:509-554）
- `dghub_port` 文本输入

l10n：`phira/locales/<lang>/settings.ftl` 加 `item-dghub` / `item-dghub-sub` /
`item-dghub-host` / `item-dghub-port`，英文先行，其余 locale 同步 key。

### 6.6 错误与生命周期

- 拉 token / 连 ws 失败：`warn!` 记录，后台任务退出，不影响游戏。
- 设备未连接（`device_info.connected=false`）：照常发 trigger，DGHub 端自行忽略。
- 退出游戏场景：drop `DghubHandle` → mpsc sender 关闭 → 后台任务 select 到 channel
  关闭后发一次礼貌性断开并退出。
- 收到 `stop`：后台任务退出；游戏继续。

---

## 7. 实施步骤清单

1. **prpr**：在 `JudgeInner` 暴露 `counts()`（[u32;4]）只读 getter（若缺）。
2. **phira/Cargo.toml**：加 `tokio-tungstenite`。
3. **phira/src/dghub.rs**：新模块（mapping / Grade / spawn / build_update_fn /
   chain_update_fns），实现握手、config 同步、trigger 发送、计数 diff。
4. **phira/src/lib.rs**：`mod dghub;`。
5. **prpr/src/config.rs**：`Config` 加 `dghub_enable/host/port` + 默认。
6. **phira/src/scene/song.rs**：构造并 chain dghub update_fn。
7. **phira/src/page/settings.rs** + `settings.ftl`：开关与 host/port 输入 + l10n。
8. **联调**：起 DGHub → 进 phira 开开关 → 打谱面，验证 Bad/Miss 触发、配置页
   改映射实时生效、断线不崩。
9. 更新本文档「实际实现」与端口默认值。

---

## 8. 待确认 / 风险

- **DGHub API 端口默认值**：文档未固定，需确认或做成纯用户填写。
- **波形预设名**：`CS2-受伤` 取自示例，需确认目标 DGHub 实例存在该预设；
  `preset` 类型字段会从主程序拉列表，配置页可选，默认值得保证存在或允许空。
- **方案 A 计数 diff**：能区分 Perfect/Good/Bad/Miss，但 **不区分** 普通 Miss 与
  Hold 漏接（都计入 Miss=index3）；默认可接受。若要精细到 Hold，改用方案 B。
- **节流**：高密度谱面 Bad/Miss 可能瞬时多发；`throttle_ms` 控制，Miss 是否节流
  做成可配置。
