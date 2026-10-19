#!/usr/bin/env python3
"""HTTPS in front of the development admin console, for the dev VM.

Usage: console-tls.py <cert.pem> <key.pem> [listen-port] [console-port]

The VM's glassd talks to a console over HTTPS only (plain HTTP is for the
insecure host backend), and in production our web servers terminate TLS in
front of glass-console (ADR 0023). This plays that part on a development
host: it listens on 127.0.0.1:<listen-port> (default 7443), which the VM
reaches as 10.0.2.2, and passes each connection to `make console-dev` on
127.0.0.1:<console-port> (default 7400). os/freebsd/scripts/enroll-dev.sh
starts it. Development only.
"""

import asyncio
import ssl
import sys


async def pipe(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, ssl.SSLError):
        pass
    finally:
        writer.close()


async def main():
    cert, key = sys.argv[1], sys.argv[2]
    listen = int(sys.argv[3]) if len(sys.argv) > 3 else 7443
    console = int(sys.argv[4]) if len(sys.argv) > 4 else 7400
    tls = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    tls.load_cert_chain(cert, key)

    async def serve(client_reader, client_writer):
        try:
            upstream_reader, upstream_writer = await asyncio.open_connection("127.0.0.1", console)
        except OSError as e:
            print(f"console-tls: the console on port {console} isn't answering: {e}", flush=True)
            client_writer.close()
            return
        await asyncio.gather(
            pipe(client_reader, upstream_writer), pipe(upstream_reader, client_writer)
        )

    server = await asyncio.start_server(serve, "127.0.0.1", listen, ssl=tls)
    print(f"console-tls: https://127.0.0.1:{listen} -> http://127.0.0.1:{console}", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
