#!/usr/bin/env python3
"""Opt-in real Cloudflare proof. Run in a disposable container (owns :443).
Requires nook, Caddy 2.11.x, cloudflared 2026.10.7, Python, Node and Vite on PATH.
Only the generated fixture is publicly exposed. Never serves repository files.
"""
import base64
import json
import os
import pathlib
import re
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.request


def until(check, seconds=90):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = check()
        if result:
            return result
        time.sleep(.25)
    raise AssertionError("live proof timed out")


def fetch(url):
    try:
        with urllib.request.urlopen(url, timeout=5) as response:
            return response.read().decode()
    except (OSError, ValueError):
        return None


def run():
    if os.environ.get("NOOK_E2E_CLOUDFLARE") != "1":
        raise SystemExit("set NOOK_E2E_CLOUDFLARE=1 explicitly; this exposes a generated fixture")
    with tempfile.TemporaryDirectory(prefix="nook-cloudflare-live-") as temporary:
        root = pathlib.Path(temporary)
        env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
                   XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"),
                   NOOK_DISABLE_UPDATE_CHECK="1")
        (root / "config/nook").mkdir(parents=True)
        protocol = os.environ.get("NOOK_TEST_TUNNEL_PROTOCOL", "auto")
        (root / "config/nook/config.toml").write_text(
            'format_version=1\ncaddy_admin="http://127.0.0.1:2019"\n'
            f'[cloudflare]\nprotocol="{protocol}"\n')
        (root / "Caddyfile").write_text(
            '{\nadmin 127.0.0.1:2019\nskip_install_trust\nauto_https disable_redirects\n}\n'
            'https://localhost {\ntls internal\nrespond 404\n}\n')
        app = root / "app"
        app.mkdir()
        (app / "index.html").write_text('<html><body>nook-public-fixture</body></html>')
        with (root / "caddy.log").open("w+") as caddy_log, (root / "nook.log").open("w+") as nook_log:
            caddy = subprocess.Popen(["caddy", "run", "--config", str(root / "Caddyfile")], env=env,
                                     stdout=caddy_log, stderr=caddy_log)
            nook = None
            connection = None
            try:
                until(lambda: fetch("http://127.0.0.1:2019/config/"), 15)
                nook = subprocess.Popen(["nook", "run", "--name", "live", "--tunnel", "cloudflare", "--", "vite"],
                                        cwd=app, env=env, stdout=nook_log, stderr=nook_log)
                registry_path = root / "state/nook/state.json"
                def url_ready():
                    if nook.poll() is not None:
                        nook_log.seek(0)
                        raise AssertionError(nook_log.read())
                    try:
                        registry = json.loads(registry_path.read_text())
                        return next((r["url"] for r in registry["tunnels"].values() if r["phase"] == "connected"), None)
                    except (OSError, ValueError, KeyError):
                        return None
                url = until(url_ready)
                until(lambda: "nook-public-fixture" in (fetch(url) or ""))
                client = until(lambda: fetch(url + "/@vite/client"))
                token = re.search(r'const wsToken = ["\']([^"\']+)', client)
                assert token, "Vite client did not expose its WebSocket session token"
                host = url.removeprefix("https://")
                connection = ssl.create_default_context().wrap_socket(socket.create_connection((host, 443), timeout=15), server_hostname=host)
                key = base64.b64encode(os.urandom(16)).decode()
                connection.sendall((f"GET /?token={token[1]} HTTP/1.1\r\nHost: {host}\r\nOrigin: {url}\r\n"
                                    f"Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n"
                                    f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Protocol: vite-hmr\r\n\r\n").encode())
                received = b""
                while b"\r\n\r\n" not in received:
                    chunk = connection.recv(4096)
                    assert chunk, "WebSocket closed before expected event"
                    received += chunk
                assert b" 101 " in received.split(b"\r\n", 1)[0], received[:200]
                while b"connected" not in received:
                    chunk = connection.recv(4096)
                    assert chunk, "WebSocket closed before expected event"
                    received += chunk
                (app / "index.html").write_text('<html><body>nook-public-fixture-updated</body></html>')
                while b"full-reload" not in received:
                    chunk = connection.recv(4096)
                    assert chunk, "WebSocket closed before expected event"
                    received += chunk
                connection.close()
                connection = None
                subprocess.run(["nook", "stop", "live"], cwd=root, env=env, check=True, capture_output=True)
                nook.wait(timeout=15)
                subprocess.run(["nook", "prune"], cwd=root, env=env, check=True, capture_output=True)
                assert not json.loads(registry_path.read_text())["tunnels"]
                until(lambda: "nook-public-fixture" not in (fetch(url) or ""), 30)
                print(f"PASS: cloudflared {protocol}, verified TLS origin, public HTTP, Vite WebSocket/HMR and cleanup")
            finally:
                if connection:
                    connection.close()
                if nook and nook.poll() is None:
                    nook.terminate()
                    try:
                        nook.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        nook.kill()
                        nook.wait()
                caddy.terminate()
                caddy.wait(timeout=10)


if __name__ == "__main__":
    run()
