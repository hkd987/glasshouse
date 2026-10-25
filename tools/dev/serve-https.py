#!/usr/bin/env python3
"""A folder over HTTPS, for the dev VM: Glasshouse's update server, played
on a development host (ADR 0061).

Usage: serve-https.py <folder> <cert.pem> <key.pem> [port]

Serves <folder> read-only on 127.0.0.1:<port> (default 7444), which the VM
reaches as 10.0.2.2, with the development certificate
os/freebsd/scripts/lib.sh's dev_tls makes. It honours a Range header from a
byte on (`bytes=N-`), as an update server or CDN does, so a download that
stopped goes on. Development only: tests/update/update-vm.sh starts it.
"""

import http.server
import os
import ssl
import sys


class Handler(http.server.SimpleHTTPRequestHandler):
    def send_head(self):
        path = self.translate_path(self.path)
        rng = self.headers.get("Range", "")
        if not rng.startswith("bytes=") or not os.path.isfile(path):
            return super().send_head()
        start = int(rng[len("bytes="):].split("-")[0] or 0)
        size = os.path.getsize(path)
        if start >= size:
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{size}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return None
        f = open(path, "rb")
        f.seek(start)
        self.send_response(206)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Range", f"bytes {start}-{size - 1}/{size}")
        self.send_header("Content-Length", str(size - start))
        self.end_headers()
        return f

    def log_message(self, fmt, *args):
        sys.stderr.write("serve-https: " + fmt % args + "\n")


def main():
    folder, cert, key = sys.argv[1], sys.argv[2], sys.argv[3]
    port = int(sys.argv[4]) if len(sys.argv) > 4 else 7444
    tls = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    tls.load_cert_chain(cert, key)
    handler = lambda *a, **k: Handler(*a, directory=folder, **k)  # noqa: E731
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    server.socket = tls.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
