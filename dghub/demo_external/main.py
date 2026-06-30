"""DGHub 外部插件示范 — 握手后触发一次演示，然后等待主程序 stop。"""
import asyncio
import json
import os
import sys

try:
    import websockets
except ImportError:
    print("缺少 websockets，请: pip install websockets", file=sys.stderr)
    raise


async def main() -> None:
    host = os.environ["DGHUB_HOST"]
    port = os.environ["DGHUB_PORT"]
    token = os.environ["DGHUB_TOKEN"]

    uri = f"ws://{host}:{port}/ws/plugin?token={token}"
    async with websockets.connect(uri) as ws:
        await ws.send(
            json.dumps(
                {
                    "op": "hello",
                    "token": token,
                    "manifest": {
                        "id": os.environ.get("DGHUB_PLUGIN_ID", "demo_external"),
                        "name": "示范插件 (Demo)",
                        "version": "0.1.0",
                        "sdk": "1",
                    },
                }
            )
        )
        ack = json.loads(await ws.recv())
        if not ack.get("accepted"):
            raise RuntimeError(ack.get("reason", "hello rejected"))

        await ws.send(
            json.dumps(
                {
                    "op": "trigger",
                    "action": "both",
                    "delta_pct": 30,
                    "strength_mode": "rollback",
                    "duration_s": 1.5,
                    "preset": "CS2-受伤",
                    "channel": "both",
                    "label": "Demo 示范触发",
                }
            )
        )

        async for raw in ws:
            if json.loads(raw).get("op") == "stop":
                return


if __name__ == "__main__":
    asyncio.run(main())
