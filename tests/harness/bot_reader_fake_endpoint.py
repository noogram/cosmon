#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Serve deterministic synthetic poll responses and record acknowledgements."""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
from urllib.parse import parse_qs, urlparse


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--response", type=Path, required=True)
    parser.add_argument("--requests", type=Path, required=True)
    parser.add_argument("--port-file", type=Path, required=True)
    parser.add_argument("--status", type=int, default=200)
    args = parser.parse_args()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            parsed = urlparse(self.path)
            query = parse_qs(parsed.query)
            acknowledgement = {
                "offset": query.get("offset", [None])[0],
                "allowed_updates": query.get("allowed_updates", [None])[0],
                "credential_is_synthetic": parsed.path.startswith("/botsynthetic-"),
            }
            with args.requests.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(acknowledgement, sort_keys=True) + "\n")
            body = args.response.read_bytes()
            self.send_response(args.status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, _format: str, *args: object) -> None:
            return

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    args.port_file.write_text(str(server.server_port), encoding="utf-8")
    server.serve_forever()


if __name__ == "__main__":
    main()
