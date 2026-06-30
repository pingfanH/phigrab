# Phira 判定联动 — DGHub 外部插件

这是一个**仅含 `manifest.json`** 的 DGHub 外部插件包，配合 Phira 内置的
DGHub 直连功能使用。

## 与 demo_external 的区别

`demo_external` 是「DGHub 启动一个 Python 进程」的典型插件，所以带 `entry: main.py`。

本插件采用 **Rust 原生直连**：Phira 游戏本体自己连 DGHub（拉 token →
`ws/plugin` → 发 `trigger`），**不需要** DGHub 来 spawn 任何进程。
因此 manifest **没有 `entry` 字段**，包里也**没有 `main.py`**。

它的唯一作用：让 DGHub 在「插件中心 → 外部插件」里认识 `id = phira` 这个插件，
并根据 `config_schema` **自动生成判定→电击的映射配置页**。你在那页改的值，
Phira 连上后通过 `config` / `config_changed` 实时收到。

## 配置项（判定 → trigger 映射）

| 分组 | 字段 | 默认 | 说明 |
|---|---|---|---|
| 通用 | channel | both | 输出通道 a/b/both |
| 通用 | throttle_ms | 80 | 同档命中节流间隔 |
| Miss | miss_enable / delta / duration / preset | 开 / 60% / 1.5s / CS2-受伤 | 漏接触发 |
| Bad | bad_enable / delta / duration / preset | 开 / 35% / 1.0s / CS2-受伤 | Bad 触发 |
| Perfect/Good | good_enable / delta、perfect_enable / delta | 关 | 默认不触发，可开 |

默认策略「只惩罚失误」：Miss / Bad 触发，Perfect / Good 关闭。

## 安装

见同目录 `../IMPORT_GUIDE.md`。简述：把本文件夹打成 zip，DGHub →
插件中心 → 外部插件 → 导入 zip。然后启动 Phira，在 Phira 设置里开
「DGHub 联动」并填对端口。
