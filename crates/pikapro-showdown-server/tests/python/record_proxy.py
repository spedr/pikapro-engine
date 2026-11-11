"""A recording websocket proxy: client <-> proxy <-> a Showdown-protocol server.

Every frame is appended to a JSON-lines file as
``{"t": seconds, "conn": n, "dir": ">" (client to server) or "<", "data": frame}``.

    python record_proxy.py --listen 22100 \
        --upstream ws://localhost:8000/showdown/websocket --out traffic.jsonl
"""

import argparse
import asyncio
import itertools
import json
import time

import websockets


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", type=int, required=True)
    parser.add_argument("--upstream", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    counter = itertools.count(1)
    out = open(args.out, "w", encoding="utf-8")
    start = time.perf_counter()

    def log(conn: int, direction: str, data: str) -> None:
        record = {"t": round(time.perf_counter() - start, 6), "conn": conn, "dir": direction, "data": data}
        out.write(json.dumps(record) + "\n")
        out.flush()

    async def handle(client):
        conn = next(counter)
        async with websockets.connect(args.upstream, max_queue=None, ping_interval=None) as upstream:

            async def pump(src, dst, direction):
                try:
                    async for message in src:
                        log(conn, direction, str(message))
                        await dst.send(message)
                except websockets.ConnectionClosed:
                    pass
                finally:
                    await dst.close()

            await asyncio.gather(pump(client, upstream, ">"), pump(upstream, client, "<"))

    async with websockets.serve(handle, "localhost", args.listen, max_queue=None, ping_interval=None):
        await asyncio.Future()


if __name__ == "__main__":
    asyncio.run(main())
