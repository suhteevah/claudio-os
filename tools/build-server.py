#!/usr/bin/env python3
"""
ClaudioOS Remote Build Server

HTTP server that accepts Rust source code, compiles it with rustc, and returns
the compilation output (errors, warnings, success status).

Runs on the host machine. The ClaudioOS kernel connects to it via QEMU's SLIRP
networking at 10.0.2.2:8445.

Usage:
    python tools/build-server.py [--port 8445] [--bind 0.0.0.0]

Endpoints:
    POST /compile
        Body: {"source": "fn main() { ... }", "target": "x86_64-claudio"}
        Response: {"success": true/false, "stdout": "...", "stderr": "..."}

    GET /health
        Response: {"status": "ok"}
"""

import json
import os
import subprocess
import sys
import tempfile
import argparse
from http.server import HTTPServer, BaseHTTPRequestHandler


class BuildHandler(BaseHTTPRequestHandler):
    """Handle compilation requests from ClaudioOS agents."""

    # Suppress default stderr logging for each request
    def log_message(self, format, *args):
        print(f"[build-server] {format % args}")

    def do_GET(self):
        if self.path == "/health":
            self._send_json({"status": "ok"})
        else:
            self._send_json({"error": "not found"}, status=404)

    def do_POST(self):
        if self.path != "/compile":
            self._send_json({"error": "not found"}, status=404)
            return

        # Read request body
        content_length = int(self.headers.get("Content-Length", 0))
        if content_length == 0:
            self._send_json({"error": "empty body"}, status=400)
            return

        try:
            raw = self.rfile.read(content_length)
            data = json.loads(raw)
        except (json.JSONDecodeError, UnicodeDecodeError) as e:
            self._send_json({"error": f"bad JSON: {e}"}, status=400)
            return

        source = data.get("source", "")
        if not source.strip():
            self._send_json({"error": "empty source"}, status=400)
            return

        # Optional fields
        edition = data.get("edition", "2021")
        mode = data.get("mode", "check")  # "check" (default) or "build"

        # Write source to a temporary file
        try:
            with tempfile.NamedTemporaryFile(
                suffix=".rs", delete=False, mode="w", encoding="utf-8"
            ) as f:
                f.write(source)
                src_path = f.name
        except OSError as e:
            self._send_json({"error": f"failed to write temp file: {e}"}, status=500)
            return

        try:
            response = self._compile(src_path, edition, mode)
            self._send_json(response)
        finally:
            # Clean up temp file
            try:
                os.unlink(src_path)
            except OSError:
                pass
            # Clean up any output binary
            out_path = src_path.replace(".rs", "")
            try:
                os.unlink(out_path)
            except OSError:
                pass
            # Windows produces .exe
            try:
                os.unlink(out_path + ".exe")
            except OSError:
                pass
            # Also .pdb on Windows
            try:
                os.unlink(out_path + ".pdb")
            except OSError:
                pass

    def _compile(self, src_path, edition, mode):
        """Run rustc on the source file and return the result."""

        if mode == "check":
            # Check only — no binary output, fastest feedback
            cmd = [
                "rustc",
                "--edition", edition,
                "--crate-type", "lib",
                "-Z", "parse-only" if False else "",  # placeholder
                src_path,
                "-o", os.devnull,
            ]
            # Simpler: just compile to /dev/null
            cmd = [
                "rustc",
                "--edition", edition,
                src_path,
                "-o", os.devnull,
            ]
        elif mode == "build":
            # Full build — produces a binary
            out_path = src_path.replace(".rs", "")
            cmd = [
                "rustc",
                "--edition", edition,
                src_path,
                "-o", out_path,
            ]
        else:
            return {"success": False, "stdout": "", "stderr": f"unknown mode: {mode}"}

        print(f"[build-server] compiling: {' '.join(cmd)}")

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=30,
            )
        except subprocess.TimeoutExpired:
            return {
                "success": False,
                "stdout": "",
                "stderr": "compilation timed out (30s limit)",
            }
        except FileNotFoundError:
            return {
                "success": False,
                "stdout": "",
                "stderr": "rustc not found — is Rust installed on the host?",
            }
        except Exception as e:
            return {
                "success": False,
                "stdout": "",
                "stderr": f"compilation error: {e}",
            }

        return {
            "success": result.returncode == 0,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }

    def _send_json(self, obj, status=200):
        """Send a JSON response."""
        body = json.dumps(obj).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser(description="ClaudioOS Remote Build Server")
    parser.add_argument("--port", type=int, default=8445, help="Listen port (default: 8445)")
    parser.add_argument("--bind", default="0.0.0.0", help="Bind address (default: 0.0.0.0)")
    args = parser.parse_args()

    server = HTTPServer((args.bind, args.port), BuildHandler)
    print(f"[build-server] listening on {args.bind}:{args.port}")
    print(f"[build-server] POST /compile  — compile Rust source")
    print(f"[build-server] GET  /health   — health check")
    print(f"[build-server] from QEMU guest, connect to 10.0.2.2:{args.port}")

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n[build-server] shutting down")
        server.shutdown()


if __name__ == "__main__":
    main()
