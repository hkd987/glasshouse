#!/usr/bin/env python3
"""pkg-mirror: the package repository an image build installs from (ADR 0022).

    pkg-mirror.py --cache DIR --upstream URL [--port-file FILE] [--offline]

Serves DIR over HTTP on 127.0.0.1 (a free port, written to --port-file),
for a build VM, which reaches it at 10.0.2.2 through QEMU's user network.
A file that isn't in DIR yet is fetched once from the same path under
--upstream, then kept: later builds use the copy, so a rerun downloads
nothing, and the repository catalog (meta.conf, data.pkg, packagesite.pkg)
stays the one the first build saw, whatever upstream publishes since. That
cache is the pinned mirror of FreeBSD's base_release repository until our
own repository exists (BS-2); release.env pins the catalog's SHA256s, and
pkg in the guest checks the catalog's signature (the base system's
pkgbase keys) and every package's checksum against it.

--offline serves only what DIR holds (404 for the rest).
"""

import argparse
import email.utils
import http.client
import http.server
import os
import shutil
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

CHUNK = 1 << 20
ATTEMPTS = 5


class Mirror(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, cache: Path, upstream: str, offline: bool):
        super().__init__(("127.0.0.1", 0), Handler)
        self.cache = cache
        self.upstream = upstream.rstrip("/")
        self.offline = offline
        self.locks: dict[str, threading.Lock] = {}
        self.locks_guard = threading.Lock()

    def lock_for(self, rel: str) -> threading.Lock:
        with self.locks_guard:
            return self.locks.setdefault(rel, threading.Lock())

    def fetch(self, rel: str, dest: Path, client: "Handler | None") -> int:
        """Downloads upstream/<rel> to dest, passing it on to client as it
        arrives (pkg gives up on a server that's silent for 30 s, and a
        kernel takes longer than that to come through); returns the HTTP
        status. A transfer cut short resumes where it stopped (a Range
        request), up to ATTEMPTS times, so the client still gets the whole
        file; the cached copy appears only once it has every byte."""
        url = f"{self.upstream}/{rel}"
        part = dest.with_name(dest.name + ".part")
        got, total = 0, None
        for attempt in range(ATTEMPTS):
            request = urllib.request.Request(url)
            if got:
                request.add_header("Range", f"bytes={got}-")
            try:
                with urllib.request.urlopen(request, timeout=120) as resp:
                    if got and resp.status != 206:
                        log(f"upstream ignored the range request for {rel}")
                        return 502
                    if total is None:
                        length = resp.headers.get("Content-Length")
                        total = int(length) if length else None
                        dest.parent.mkdir(parents=True, exist_ok=True)
                        part.unlink(missing_ok=True)
                        if client:
                            client.send_headers(length, time.time())
                    with open(part, "ab") as out:
                        while True:
                            try:
                                chunk = resp.read(CHUNK)
                            except http.client.IncompleteRead as e:
                                chunk = e.partial
                                if not chunk:
                                    break
                            if not chunk:
                                break
                            out.write(chunk)
                            got += len(chunk)
                            if client:
                                try:
                                    client.wfile.write(chunk)
                                except OSError:
                                    client = None  # gone; finish the cached copy anyway
            except urllib.error.HTTPError as e:
                log(f"upstream {e.code} for {rel}")
                if total is None:
                    return e.code
            except (urllib.error.URLError, TimeoutError, OSError, http.client.HTTPException) as e:
                log(f"upstream failed for {rel}: {e}")
            if total is None or got == total:
                break
            log(f"{rel}: {got} of {total} bytes; resuming (attempt {attempt + 2} of {ATTEMPTS})")
        if total is None and got == 0:
            return 502
        if total is not None and got != total:
            log(f"{rel}: gave up at {got} of {total} bytes")
            part.unlink(missing_ok=True)
            return 502
        part.rename(dest)
        log(f"fetched {rel} ({got // 1024} KiB)")
        return 200


class Handler(http.server.BaseHTTPRequestHandler):
    server: Mirror

    def resolve(self) -> tuple[str, Path] | None:
        rel = self.path.split("?", 1)[0].lstrip("/")
        parts = rel.split("/")
        if not rel or any(p in ("", ".", "..") for p in parts):
            return None
        return rel, self.server.cache.joinpath(*parts)

    def do_GET(self) -> None:  # noqa: N802 (http.server's name)
        self.serve(body=True)

    def do_HEAD(self) -> None:  # noqa: N802
        self.serve(body=False)

    def serve(self, body: bool) -> None:
        found = self.resolve()
        if found is None:
            self.send_error(400)
            return
        rel, path = found
        if not path.is_file():
            if self.server.offline:
                self.send_error(404)
                return
            with self.server.lock_for(rel):
                if not path.is_file():
                    status = self.server.fetch(rel, path, self if body else None)
                    if self.sent:
                        return  # sent as it arrived (or cut off: the client sees a short file)
                    if status != 200:
                        self.send_error(status if status in (403, 404, 410) else 502)
                        return
        st = path.stat()
        self.send_headers(str(st.st_size), st.st_mtime)
        if body:
            with open(path, "rb") as f:
                shutil.copyfileobj(f, self.wfile, CHUNK)

    sent = False

    def send_headers(self, length: str | None, mtime: float) -> None:
        self.sent = True
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        if length is not None:
            self.send_header("Content-Length", length)
        # pkg records it for the catalog (and warns without it).
        self.send_header("Last-Modified", email.utils.formatdate(mtime, usegmt=True))
        self.end_headers()

    def log_message(self, format: str, *args) -> None:  # noqa: A002 (base class name)
        pass


def log(msg: str) -> None:
    print(f"[pkg-mirror] {msg}", file=sys.stderr, flush=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cache", type=Path, required=True)
    ap.add_argument("--upstream", required=True)
    ap.add_argument("--port-file", type=Path)
    ap.add_argument("--offline", action="store_true")
    args = ap.parse_args()

    args.cache.mkdir(parents=True, exist_ok=True)
    server = Mirror(args.cache.resolve(), args.upstream, args.offline)
    port = server.server_address[1]
    if args.port_file:
        tmp = args.port_file.with_name(args.port_file.name + ".tmp")
        tmp.write_text(f"{port}\n")
        os.replace(tmp, args.port_file)
    log(f"serving {args.cache} on 127.0.0.1:{port}{' (offline)' if args.offline else ''}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
