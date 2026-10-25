#!/usr/bin/env python3
"""Usage: tools/dev/model-relay.py [port]     (default 8787)

Anthropic's API for the dev VM from a cloud session. The VM can't reach it
itself: the session's egress proxy re-signs TLS, and the VM never trusts
that. This relay listens on 127.0.0.1 (forward it into the VM with
`ssh -R`), and passes each request on to https://api.anthropic.com from
this machine, which trusts the proxy.

The key is put in here, from GLASS_ANTHROPIC_API_KEY (never
ANTHROPIC_API_KEY): the VM's secrets file holds a placeholder, so the key
never enters the VM. It's never printed. glassd uses the relay only where
its glassd.toml says `model_relay = true` and policy names the relay as
the provider's endpoint (`http://127.0.0.1:<port>`).

One line per request on stderr: the path, the status and the time. No
headers, no bodies.
"""

import http.server
import os
import sys
import time
import urllib.error
import urllib.request

UPSTREAM = "https://api.anthropic.com"
# What the API reads; anything else stays here.
PASSED = ("content-type", "anthropic-version", "anthropic-beta", "accept")


class Relay(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self) -> None:
        started = time.monotonic()
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length)
        headers = {k: v for k, v in self.headers.items() if k.lower() in PASSED}
        headers["x-api-key"] = KEY
        request = urllib.request.Request(UPSTREAM + self.path, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=600) as reply:
                status, data, kind = reply.status, reply.read(), reply.headers.get("content-type", "application/json")
        except urllib.error.HTTPError as e:
            status, data, kind = e.code, e.read(), e.headers.get("content-type", "application/json")
        except OSError as e:
            status, data, kind = 502, f'{{"error": {{"type": "relay", "message": "{type(e).__name__}"}}}}'.encode(), "application/json"
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        print(f"model-relay: POST {self.path} -> {status} in {time.monotonic() - started:.1f}s", file=sys.stderr, flush=True)

    def log_message(self, *_: object) -> None:
        pass


if __name__ == "__main__":
    KEY = os.environ.get("GLASS_ANTHROPIC_API_KEY", "")
    if not KEY:
        sys.exit("model-relay: GLASS_ANTHROPIC_API_KEY isn't set")
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Relay)
    print(f"model-relay: on 127.0.0.1:{port}", file=sys.stderr, flush=True)
    server.serve_forever()
