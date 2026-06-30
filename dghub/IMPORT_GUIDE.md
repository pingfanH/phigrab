# 在 DGHub 中导入 Phira 判定联动插件

本指南说明如何把「Phira 判定联动」装进 DGHub，并与 Phira 游戏本体联动。
参考实现的目录与字段格式对齐 `demo_external/`，协议依据 `PLUGIN_DEVELOPMENT.md`。

---

## 0. 前置

- 已安装并能运行 **DGHub** 主程序（插件中心可用）。
- 已编译好的 **Phira**（带本仓库的 DGHub 联动改动）。
- DGHub 已连上电击设备（`device_info.connected = true`）。

---

## 1. 架构（先读一遍，避免装错）

```
Phira(游戏本体) ── 自己拉 token、连 ws ──▶ DGHub 主程序 ──▶ 电击设备
        │                                      ▲
        └ 每次 Perfect/Good/Bad/Miss 判定        │
          按映射发 trigger ────────────────────┘

DGHub 里的「插件中心」只用本插件包的 manifest 来：
  · 显示插件「Phira 判定联动」(id = phira)
  · 自动生成判定→电击的映射配置页
它不会去启动 Phira（manifest 无 entry）。Phira 由你手动开。
```

要点：**这是直连架构**。插件包里没有 `main.py` / `entry`，DGHub 不 spawn 任何东西。
Phira 自己作为 `id = phira` 的插件接入。

---

## 2. 打包成 zip

把 `phira_plugin/` 目录打成 zip。zip 根目录直接放文件，**或**整个文件夹包在
唯一一级子目录里——两种 DGHub 都支持。

macOS / Linux：

```bash
cd dghub/phira_plugin
zip -r ../phira_plugin.zip manifest.json README.md
# 生成 dghub/phira_plugin.zip
```

Windows（PowerShell）：

```powershell
Compress-Archive -Path manifest.json,README.md -DestinationPath ..\phira_plugin.zip
```

> 包里**必须**有 `manifest.json`（在 zip 根或唯一子目录内）。`README.md` 可选。

---

## 3. 导入 DGHub

1. 打开 DGHub → **插件中心 → 外部插件**。
2. 点 **「导入 zip」**，选刚才的 `phira_plugin.zip`。
3. 列表里出现 **「Phira 判定联动」**（id `phira`）。

> 也可以不打 zip：直接把 `phira_plugin/` 解压/复制进 DGHub 的 `plugins/` 目录，
> 重启 DGHub 即可看到（与文档 §1 的最小示例同理）。

---

## 4. 在 DGHub 里配置映射

点开「Phira 判定联动」的配置页（DGHub 依据 manifest 的 `config_schema` 自动生成）：

- **通用**：输出通道 a/b/both、节流 ms。
- **Miss / Bad**：开关、强度(%)、时长(s)、波形预设。默认 Miss 60% / Bad 35%。
- **Perfect / Good**：默认关闭，想要「打准也有反馈」可打开并设强度。

> **波形预设**：`miss_preset` / `bad_preset` 默认填 `CS2-受伤`。请确认你的 DGHub
> 里**存在**这个预设名（配置页的 preset 下拉会列出主程序里的预设）；不存在就改成
> 你有的预设名，或留空（留空时该档只改强度、不播波形）。

改完即生效——Phira 连上后会实时收到 `config` / `config_changed`。

---

## 5. 在 Phira 里开启联动

1. 启动 Phira。
2. **设置 → 通用**，往下找 **「DGHub 联动」**：
   - 打开开关；
   - **DGHub 主机**：一般 `127.0.0.1`；
   - **DGHub 端口**：填 DGHub 的 API 端口，**必须与 DGHub 设置里的端口一致**
     （Phira 默认 `8765`，按你的实际改）。
3. 进入任意谱面开打。

Phira 在进入游戏时会：拉 `GET /api/plugins/_session_token` 取 token →
连 `ws://主机:端口/ws/plugin?token=...` → 发 `hello`（id `phira`）→ 握手成功后，
每次判定按 DGHub 配置页的映射发 `trigger`。

---

## 6. 验证联动成功

- DGHub 插件列表里「Phira 判定联动」显示为**运行中 / 已连接**（Phira 在游戏中时）。
- 故意漏几个音符（Miss）/ 打歪（Bad）——设备应触发电击，时长/强度符合配置。
- 在 DGHub 配置页调大 Miss 强度，回 Phira 再 Miss，强度应随之变化（实时生效）。

---

## 7. 排错

| 现象 | 原因 / 处理 |
|---|---|
| Phira 列表里一直「未运行」 | Phira 没进游戏，或端口填错。直连只在**游戏进行中**保持连接。 |
| 完全不触发 | 端口不一致；或该判定档 `enable=false`；或谱面没产生 Bad/Miss。 |
| 触发但不震只改强度 | 对应档 `preset` 为空或预设名不存在——填一个 DGHub 里真实存在的预设。 |
| 高密度段落电不停 | 调大 `throttle_ms`。 |
| 设备没反应 | DGHub 未连设备（`device_info.connected=false`），或设备强度上限为 0。 |
| Phira 日志报连接失败 | DGHub 没开、端口错、或 `/api/plugins/_session_token` 拿不到 token。 |

> 断线不影响游戏：Phira 只 `warn!` 记录，下次进游戏重连。

---

## 8. 文件清单

```
dghub/
  phira_plugin/
    manifest.json   # 必需：id=phira + 判定映射 config_schema（无 entry）
    README.md       # 说明
  PHIRA_INTEGRATION.md  # 设计文档（架构、协议、实现）
  IMPORT_GUIDE.md       # 本文件
```
