#!/usr/bin/env python3
"""Local forwarder that records the body sent toward Ollama.

It binds on all interfaces so a container can reach a loopback-only Ollama.
The recorded body is the forwarded context. It is not a quality judgment.
"""
from __future__ import annotations

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import urllib.error
import urllib.request

UPSTREAM = os.environ.get("OLLAMA_UPSTREAM", "http://127.0.0.1:11434")
PORT = int(os.environ.get("PROXY_PORT", "11435"))
LOG = os.environ.get("PROXY_LOG", "")


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        return

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        if LOG:
            with open(LOG, "a", encoding="utf-8") as fh:
                fh.write(json.dumps({"path": self.path, "bytes": len(raw), "body": raw.decode("utf-8", "replace")}) + "\n")
        req = urllib.request.Request(UPSTREAM + self.path, data=raw, method="POST")
        req.add_header("Content-Type", self.headers.get("Content-Type", "application/json"))
        try:
            with urllib.request.urlopen(req, timeout=180) as resp:
                body = resp.read()
                code = resp.status
                ctype = resp.headers.get("Content-Type", "application/json")
        except urllib.error.HTTPError as exc:
            body = exc.read()
            code = exc.code
            ctype = "application/json"
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
