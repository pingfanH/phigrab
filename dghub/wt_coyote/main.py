import asyncio
import json
import os
import sys
import time
import urllib.request
import logging

try:
    import websockets
except ImportError:
    print("缺少 websockets，请在终端执行: pip install websockets", file=sys.stderr)
    sys.exit(1)

logging.basicConfig(format="%(asctime)s - [%(levelname)s]: %(message)s", level=logging.INFO)

plugin_cfg = {}
send_queue = asyncio.Queue()

# 全局环境变数与锁 (修复了首局偏移误差，初始值设为 0)
hard_offset = 0  
current_baseline_a = 0
current_baseline_b = 0
channel_locks = {'a': (0, 0.0), 'b': (0, 0.0)}  

# 网络请求工具
def fetch_json(url):
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
        with urllib.request.urlopen(req, timeout=0.5) as res:
            return json.loads(res.read().decode('utf-8'))
    except Exception:
        return None

# ===============================
# 核心事件派发引擎 (带优先级与A/B分离)
# ===============================
async def dispatch_event(event_id, label_prefix):
    global hard_offset
    
    if not plugin_cfg.get(f"{event_id}_enable", False):
        return

    # 1. 处理困难模式永久强度累加
    mode = plugin_cfg.get("difficulty_mode", "simple")
    if mode == "hard":
        if event_id == "evt_death": hard_offset += plugin_cfg.get("hard_death_add", 2)
        elif event_id == "evt_kill": hard_offset -= plugin_cfg.get("hard_kill_sub", 1)
        elif event_id == "evt_lose": hard_offset += plugin_cfg.get("hard_lose_add", 3)
        elif event_id == "evt_win": hard_offset -= plugin_cfg.get("hard_win_sub", 3)

    # 2. 读取当前事件参数 (分离AB基础强度乘区)
    pct_a = plugin_cfg.get(f"{event_id}_delta_a", 0)
    pct_b = plugin_cfg.get(f"{event_id}_delta_b", 0)
    dur = plugin_cfg.get(f"{event_id}_dur", 1.0)
    preset = plugin_cfg.get(f"{event_id}_preset", "")
    priority = plugin_cfg.get(f"{event_id}_priority", 1)
    
    base_s_a = plugin_cfg.get("ui_base_a", 5)
    base_s_b = plugin_cfg.get("ui_base_b", 5)
    
    # 系数转换：各通道独立基础强度 * 百分比
    delta_a = int(base_s_a * (pct_a / 100.0))
    delta_b = int(base_s_b * (pct_b / 100.0))
    
    now = time.time()
    can_trigger_a = (priority >= channel_locks['a'][0] or now >= channel_locks['a'][1])
    can_trigger_b = (priority >= channel_locks['b'][0] or now >= channel_locks['b'][1])

    # 3. 拦截与并轨发送
    if not can_trigger_a and not can_trigger_b:
        return 
        
    logging.info(f"⚡ 派发事件 [{label_prefix}] (Pri:{priority}) | A临时增幅:{delta_a} B临时增幅:{delta_b}")

    if can_trigger_a and can_trigger_b and delta_a == delta_b:
        channel_locks['a'] = (priority, now + dur)
        channel_locks['b'] = (priority, now + dur)
        if delta_a != 0 or preset:
            await send_queue.put({
                "op": "trigger", "action": "both" if preset else "strength",
                "strength_mode": "rollback", "delta_pct": delta_a,
                "duration_s": dur, "preset": preset, "channel": "both",
                "label": label_prefix
            })
    else:
        if can_trigger_a and (delta_a != 0 or preset):
            channel_locks['a'] = (priority, now + dur)
            await send_queue.put({
                "op": "trigger", "action": "both" if preset else "strength",
                "strength_mode": "rollback", "delta_pct": delta_a,
                "duration_s": dur, "preset": preset, "channel": "a",
                "label": f"{label_prefix}(A)"
            })
        if can_trigger_b and (delta_b != 0 or preset):
            channel_locks['b'] = (priority, now + dur)
            await send_queue.put({
                "op": "trigger", "action": "both" if preset else "strength",
                "strength_mode": "rollback", "delta_pct": delta_b,
                "duration_s": dur, "preset": preset, "channel": "b",
                "label": f"{label_prefix}(B)"
            })

# ===============================
# 任务1：遥测状态追踪与环境基准更新
# ===============================
async def indicators_task():
    global current_baseline_a, current_baseline_b
    
    last_crew = -1
    last_track = False
    last_ammo = -1
    last_lws = False
    
    while True:
        await asyncio.sleep(plugin_cfg.get("polling_rate", 200) / 1000.0)
        data = await asyncio.to_thread(fetch_json, "http://127.0.0.1:8111/indicators")
        if not data: continue
        
        is_tank = "crew_total" in data
        speed_kmh = abs(data.get("speed", 0.0)) * 3.6
        
        if is_tank:
            cur_crew = data.get("crew_current", -1)
            if last_crew != -1 and cur_crew != -1 and cur_crew < last_crew:
                await dispatch_event("evt_crew", "乘员阵亡")
            last_crew = cur_crew

            cur_track = data.get("track_broken", False)
            if cur_track and not last_track:
                await dispatch_event("evt_track", "履带断裂")
            last_track = cur_track

            cur_ammo = data.get("first_stage_ammo", -1)
            if last_ammo != -1 and cur_ammo != -1 and cur_ammo < last_ammo:
                await dispatch_event("evt_ammo", "开火/待发消耗")
            last_ammo = cur_ammo

            cur_lws = data.get("lws", 0.0) == 1.0 or data.get("lws", False) is True
            if cur_lws and not last_lws:
                await dispatch_event("evt_lws", "激光告警(LWS)")
            last_lws = cur_lws

        # 速度环境加成
        speed_bonus = 0
        if is_tank and plugin_cfg.get("speed_ground_enable", True):
            step = plugin_cfg.get("speed_ground_step", 10)
            if step > 0: speed_bonus = int(speed_kmh / step) * plugin_cfg.get("speed_ground_add", 1)
        elif not is_tank and plugin_cfg.get("speed_air_enable", False):
            step = plugin_cfg.get("speed_air_step", 50)
            if step > 0: speed_bonus = int(speed_kmh / step) * plugin_cfg.get("speed_air_add", 1)

        # 独立计算 AB 通道的最终静态强度
        base_s_a = plugin_cfg.get("ui_base_a", 5)
        base_s_b = plugin_cfg.get("ui_base_b", 5)
        max_s_a = plugin_cfg.get("ui_max_a", 50)
        max_s_b = plugin_cfg.get("ui_max_b", 50)
        
        target_s_a = max(0, min(base_s_a + speed_bonus + hard_offset, max_s_a))
        target_s_b = max(0, min(base_s_b + speed_bonus + hard_offset, max_s_b))
        
        delta_a = target_s_a - current_baseline_a
        delta_b = target_s_b - current_baseline_b
        
        if delta_a != 0 or delta_b != 0:
            if delta_a == delta_b:
                await send_queue.put({
                    "op": "trigger", "action": "strength", "strength_mode": "permanent",
                    "delta_pct": delta_a, "channel": "both",
                    "label": "全局环境基准同步"
                })
            else:
                if delta_a != 0:
                    await send_queue.put({
                        "op": "trigger", "action": "strength", "strength_mode": "permanent",
                        "delta_pct": delta_a, "channel": "a",
                        "label": "环境基准同步(A)"
                    })
                if delta_b != 0:
                    await send_queue.put({
                        "op": "trigger", "action": "strength", "strength_mode": "permanent",
                        "delta_pct": delta_b, "channel": "b",
                        "label": "环境基准同步(B)"
                    })
            current_baseline_a = target_s_a
            current_baseline_b = target_s_b

# ===============================
# 任务2：HUD事件突发触发
# ===============================

async def hudmsg_task():
    last_dmg_id = 0
    while True:
        await asyncio.sleep(plugin_cfg.get("polling_rate", 200) / 1000.0)
        pid = plugin_cfg.get("player_id", "")
        if not pid: continue
            
        data = await asyncio.to_thread(fetch_json, f"http://127.0.0.1:8111/hudmsg?lastEvt=0&lastDmg={last_dmg_id}")
        if not data: continue
        
        msgs_to_process = []
        if isinstance(data, dict):
            for k, msg_list in data.items():
                if isinstance(msg_list, list):
                    msgs_to_process.extend([m for m in msg_list if isinstance(m, dict)])
        elif isinstance(data, list):
            msgs_to_process = [m for m in data if isinstance(m, dict)]

        for d in msgs_to_process:
            raw_msg = d.get("msg", "")
            did = d.get("id", last_dmg_id)
            if did > last_dmg_id: last_dmg_id = did
            
            # 【终极清洗】用正则干掉所有零宽字符和控制符
            msg = re.sub(r'[\u200b-\u200f\u202a-\u202e\ufeff]', '', raw_msg)
            if pid not in msg: continue
                
            evt_type = None
            
            # 【一刀切分割法】精准判断施暴者和受害者
            def check_role(keywords):
                for kw in keywords:
                    if kw in msg:
                        parts = msg.split(kw, 1) # 从关键词处一刀切成左右两半
                        if len(parts) == 2:
                            in_left = pid in parts[0]
                            in_right = pid in parts[1]
                            if in_left and not in_right: return "attacker"
                            if in_right and not in_left: return "victim"
                            if in_left and in_right: return "both" # ID太短导致两边都有匹配
                return None

            # 1. 优先判断击杀/阵亡
            role_kill = check_role(["击毁了", "击毁", "击落了", "击落"])
            if role_kill == "attacker": 
                evt_type = "kill"
            elif role_kill in ["victim", "both"]: 
                evt_type = "death" # 如果ID太短导致两边都有，为了安全防漏，视为自己阵亡
                
            # 2. 判断受击
            if not evt_type:
                role_crit = check_role(["致命攻击", "重创"])
                if role_crit in ["victim", "both"]: evt_type = "crit"
                
            # 3. 判断起火
            if not evt_type:
                role_fire = check_role(["点燃了", "引燃了"])
                if role_fire in ["victim", "both"]: evt_type = "fire"
                
            # 4. 判断坠毁
            if not evt_type:
                role_crash = check_role(["已坠毁", "坠毁"])
                if role_crash: evt_type = "crash"

            if evt_type:
                labels = {
                    "kill": "🎯击杀敌人", 
                    "death": "💀阵亡被毁", 
                    "crit": "⚔️严重受击", 
                    "fire": "🔥被点燃", 
                    "crash": "💥自爆坠机"
                }
                await dispatch_event(f"evt_{evt_type}", labels[evt_type])

# ===============================
# 任务3：战局胜负与聊天指令
# ===============================
async def mission_chat_task():
    global hard_offset
    last_chat_id = 0
    prev_status = "unknown"
    
    while True:
        await asyncio.sleep(plugin_cfg.get("polling_rate", 200) / 1000.0)
        
        # 1. 战局检查
        m_data = await asyncio.to_thread(fetch_json, "http://127.0.0.1:8111/mission.json") or {}
        cur_status = m_data.get("status", "unknown")
        
        if cur_status == "running" and prev_status != "running":
            logging.info("🎮 检测到进入新战局，正在初始化参数...")
            hard_offset = 0 
            idle_preset = plugin_cfg.get("ui_idle_preset", "")
            if idle_preset:
                await send_queue.put({"op": "pulse", "preset": idle_preset, "channel": "both"})
                
        if prev_status == "running":
            if cur_status in ["fail", "unknown"]:
                await dispatch_event("evt_lose", "战局败北/逃脱")
            elif cur_status == "success":
                await dispatch_event("evt_win", "战局胜利")
        prev_status = cur_status
        
        # 2. 聊天指令
        if not plugin_cfg.get("evt_chat_enable", True): continue
        c_data = await asyncio.to_thread(fetch_json, f"http://127.0.0.1:8111/gamechat?lastId={last_chat_id}") or []
        if isinstance(c_data, list):
            for msg in c_data:
                if isinstance(msg, dict):
                    last_chat_id = max(last_chat_id, msg.get("id", 0))
                    text = msg.get("msg", "")
                    kw = plugin_cfg.get("chat_cmd_keyword", "进攻D点")
                    if kw and kw in text:
                        await dispatch_event("evt_chat", f"聊天指令触发")

# ===============================
# WebSocket 及参数黑科技清洗
# ===============================

OLD_KEY_MAPPING = {
    "ui_speed_a": "ui_speed_rate", "ui_cd_a": "ui_cd_rate", "ui_ammo_a": "ui_ammo_rate", 
    "ui_lws_a": "ui_lws_rate", "ui_vd_a": "ui_vd_rate"
}

TYPE_RULES = {
    "bool": ["_enable", "trigger_import", "trigger_export"],
    "float": ["_dur", "_rate"],
    "str": ["_preset", "player_id", "preset_file_path", "difficulty_mode", "chat_cmd_keyword"],
    "int": ["_delta_a", "_delta_b", "_priority", "_add", "_sub", "polling_rate", "_step", "_base_a", "_base_b", "_max_a", "_max_b"]
}

def auto_cast(k, v):
    for t, suffixes in TYPE_RULES.items():
        if any(k.endswith(s) or k == s for s in suffixes):
            try:
                if t == "bool": return str(v).lower() in ["true", "1", "yes"]
                if t == "float": return float(v)
                if t == "str": return str(v)
                if t == "int": return int(float(v))
            except: pass
    return v

async def rx_loop(ws):
    async for raw in ws:
        try:
            data = json.loads(raw)
            op = data.get("op")
            
            if op == "stop": return "stop"
            elif op == "config":
                plugin_cfg.update(data.get("data", {}))
                logging.info("✅ 初始配置加载完毕")
            elif op == "config_changed":
                key = data.get("key")
                val = data.get("value")
                plugin_cfg[key] = val

                if key == "trigger_import" and val is True:
                    async def reset_import_switch():
                        await asyncio.sleep(1.5)
                        await send_queue.put({"op": "set_config", "key": "trigger_import", "value": False})
                    asyncio.create_task(reset_import_switch())
                    
                    filepath = str(plugin_cfg.get("preset_file_path", "")).strip('"').strip("'")
                    if os.path.exists(filepath):
                        try:
                            with open(filepath, "r", encoding="utf-8") as f:
                                preset_data = json.load(f)
                            imported_count = 0
                            for pk, pv in preset_data.items():
                                if pk in ["preset_file_path", "trigger_import", "trigger_export"]: continue 
                                if pk in OLD_KEY_MAPPING: pk = OLD_KEY_MAPPING[pk]
                                
                                # 向后兼容：如果导入的旧预设含有老的 "_ab" 合并参数，自动拆分应用到 A和B
                                if pk == "ui_base_ab":
                                    pv_cast = auto_cast("ui_base_a", pv)
                                    plugin_cfg["ui_base_a"] = pv_cast
                                    plugin_cfg["ui_base_b"] = pv_cast
                                    await send_queue.put({"op": "set_config", "key": "ui_base_a", "value": pv_cast})
                                    await send_queue.put({"op": "set_config", "key": "ui_base_b", "value": pv_cast})
                                    imported_count += 2
                                    continue
                                elif pk == "ui_max_ab":
                                    pv_cast = auto_cast("ui_max_a", pv)
                                    plugin_cfg["ui_max_a"] = pv_cast
                                    plugin_cfg["ui_max_b"] = pv_cast
                                    await send_queue.put({"op": "set_config", "key": "ui_max_a", "value": pv_cast})
                                    await send_queue.put({"op": "set_config", "key": "ui_max_b", "value": pv_cast})
                                    imported_count += 2
                                    continue
                                
                                pv = auto_cast(pk, pv) 
                                plugin_cfg[pk] = pv
                                await send_queue.put({"op": "set_config", "key": pk, "value": pv})
                                imported_count += 1
                                await asyncio.sleep(0.01)
                            await send_queue.put({"op": "log", "level": "info", "message": f"📥 导入完成 ({imported_count}项)！请退出重进本设置页以刷新滑块。"})
                        except Exception as e: logging.error(f"❌ 导入错误: {e}")

                elif key == "trigger_export" and val is True:
                    async def reset_export_switch():
                        await asyncio.sleep(1.5)
                        await send_queue.put({"op": "set_config", "key": "trigger_export", "value": False})
                    asyncio.create_task(reset_export_switch())
                    
                    filepath = str(plugin_cfg.get("preset_file_path", "")).strip('"').strip("'")
                    if filepath:
                        try:
                            export_data = {k: v for k, v in plugin_cfg.items() if k not in ["trigger_import", "trigger_export"]}
                            dir_name = os.path.dirname(filepath)
                            if dir_name and not os.path.exists(dir_name): os.makedirs(dir_name, exist_ok=True)
                            with open(filepath, "w", encoding="utf-8") as f:
                                f.write(json.dumps(export_data, ensure_ascii=False, indent=4))
                                f.flush()
                                os.fsync(f.fileno())
                            await send_queue.put({"op": "log", "level": "info", "message": f"📤 导出成功！"})
                        except Exception as e: logging.error(f"❌ 导出失败: {e}")

        except Exception as e: logging.error(f"❌ 消息处理异常: {e}")

async def tx_loop(ws):
    while True:
        try: await ws.send(json.dumps(await send_queue.get()))
        except Exception: pass

def get_latest_token(host, port):
    try:
        req = urllib.request.Request(f"http://{host}:{port}/api/plugins/_session_token", headers={'User-Agent': 'Mozilla/5.0'})
        with urllib.request.urlopen(req, timeout=1.0) as res:
            d = res.read().decode('utf-8')
            try: return json.loads(d).get("token", d.strip(' "'))
            except Exception: return d.strip(' "')
    except Exception: 
        return None

async def try_connect(host, port, manifest_data):
    token = get_latest_token(host, port)
    if not token:
        return False
        
    uri = f"ws://{host}:{port}/ws/plugin?token={token}"
    try:
        async with websockets.connect(uri, close_timeout=1) as ws:
            await ws.send(json.dumps({
                "op": "hello",
                "token": token,
                "manifest": manifest_data
            }))
            raw_ack = await asyncio.wait_for(ws.recv(), timeout=1.5)
            ack = json.loads(raw_ack)
            if ack.get("op") == "hello_ack" and ack.get("accepted"):
                return token
            elif ack.get("op") == "hello_ack" and not ack.get("accepted") and ack.get("reason") == "unknown_plugin_id":
                logging.error(f"❌ 端口 {port} 连接成功，但插件未在 DGHub 注册！请先将 zip 导入 DGHub 外部插件！")
                sys.exit(1)
    except Exception:
        pass
    return False

async def main():
    host = os.environ.get("DGHUB_HOST", "127.0.0.1")
    env_port = os.environ.get("DGHUB_PORT")

    manifest_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "manifest.json")
    try:
        with open(manifest_path, "r", encoding="utf-8") as f: manifest_data = json.load(f)
    except Exception as e:
        logging.error(f"❌ 读取 manifest.json 失败！请确保它和 main.py 在同一个文件夹里。({e})")
        sys.exit(1)

    active_port = None
    active_token = None
    
    token = os.environ.get("DGHUB_TOKEN")
    if token:
        active_port = env_port or "8000"
        active_token = token
    else:
        logging.info("本地独立运行：开始自动扫描 DGHub 端口...")
        ports_to_try = [8000]
        if env_port and env_port.isdigit():
            ports_to_try.insert(0, int(env_port))
        ports_to_try = list(dict.fromkeys(ports_to_try)) 
        
        for p in ports_to_try:
            logging.info(f"📡 正在探测端口 {p} ...")
            found_token = await try_connect(host, p, manifest_data)
            if found_token:
                active_port = p
                active_token = found_token
                break

    if not active_token:
        logging.error("❌ 无法找到 DGHub 的正确端口或无法连接。请确保 DGHub 正在运行！")
        sys.exit(1)
        
    logging.info(f"✅ 成功锁定 DGHub！(端口: {active_port})")

    uri = f"ws://{host}:{active_port}/ws/plugin?token={active_token}"
    async with websockets.connect(uri) as ws:
        await ws.send(json.dumps({"op": "hello", "token": active_token, "manifest": manifest_data}))
        raw_ack = await ws.recv()
        try: ack = json.loads(raw_ack)
        except: return
        if not ack.get("accepted"): 
            logging.error(f"❌ 握手被拒绝! 服务器说: {ack.get('reason')}")
            return
            
        logging.info("✅ V3 完全体初始化成功！等待进入战局...")
        t_rx = asyncio.create_task(rx_loop(ws))
        t_tx = asyncio.create_task(tx_loop(ws))
        t_ind = asyncio.create_task(indicators_task())
        t_hud = asyncio.create_task(hudmsg_task())
        t_miss = asyncio.create_task(mission_chat_task())
        
        await t_rx
        
        t_tx.cancel()
        t_ind.cancel()
        t_hud.cancel()
        t_miss.cancel()

if __name__ == "__main__":
    asyncio.run(main())