#!/usr/bin/env python3
"""Opt-in real ngrok proof. Run in a disposable container (owns :443).
Requires nook, Caddy 2.11.x, ngrokd 2026.10.7, Python, Node and Vite on PATH.
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
import urllib.error


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
        with urllib.request.urlopen(urllib.request.Request(url, headers={"ngrok-skip-browser-warning":"1"}), timeout=5) as response:
            return response.read().decode()
    except (OSError, ValueError):
        return None


def run():
    if os.environ.get("NOOK_E2E_NGROK") != "1":
        raise SystemExit("set NOOK_E2E_NGROK=1 explicitly; this exposes a generated fixture")
    if os.environ.get("NGROK_TEST_TOKEN_FILE"):
        os.environ["NGROK_AUTHTOKEN"] = pathlib.Path(os.environ["NGROK_TEST_TOKEN_FILE"]).read_text().strip()
    with tempfile.TemporaryDirectory(prefix="nook-ngrok-live-") as temporary:
        root = pathlib.Path(temporary)
        env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
                   XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"),
                   NOOK_DISABLE_UPDATE_CHECK="1")
        (root / "config/nook").mkdir(parents=True)
        (root / "config/nook/config.toml").write_text(
            'format_version=1\ncaddy_admin="http://127.0.0.1:2019"\n'
            '[ngrok]\nstartup_timeout_seconds=60\n')
        (root / "Caddyfile").write_text(
            '{\nadmin 127.0.0.1:2019\nskip_install_trust\nauto_https disable_redirects\n}\n'
            'https://localhost {\ntls internal\nrespond "tls-fixture"\n}\n')
        app = root / "app"
        app.mkdir()
        (app / "index.html").write_text('<html><body>nook-public-fixture</body></html>')
        (app / "vite.config.mjs").write_text("""
import { createHmac } from 'node:crypto';
export default { plugins: [{ name:'fixture-webhook', configureServer(server) {
  server.middlewares.use('/webhook', (req,res) => {
    const chunks=[]; req.on('data',c=>chunks.push(c)); req.on('end',()=>{
      const body=Buffer.concat(chunks);
      const valid=createHmac('sha256','fixture-key').update(body).digest('hex')===req.headers['x-signature'];
      res.statusCode=valid?200:403; res.end(valid?'webhook-ok':'invalid');
    });
  });
}}] };
""")
        with (root / "caddy.log").open("w+") as caddy_log, (root / "nook.log").open("w+") as nook_log:
            caddy = subprocess.Popen(["caddy", "run", "--config", str(root / "Caddyfile")], env=env,
                                     stdout=caddy_log, stderr=caddy_log)
            nook = None
            connection = None
            try:
                until(lambda: fetch("http://127.0.0.1:2019/config/"), 15)
                nook = subprocess.Popen(["nook", "run", "--name", "live", "--tunnel", "ngrok", "--", "vite"],
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
                import hashlib, hmac
                payload = b'\x00nook-webhook\xff'
                signature = hmac.new(b'fixture-key', payload, hashlib.sha256).hexdigest()
                request = urllib.request.Request(url + '/webhook?fixture=1', data=payload,
                    headers={'X-Signature':signature,'Content-Type':'application/octet-stream','ngrok-skip-browser-warning':'1'})
                with urllib.request.urlopen(request, timeout=10) as response:
                    assert response.read() == b'webhook-ok'
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
                ca = root / 'caddy-root.pem'
                subprocess.run(['nook','ca','export',str(ca)],env=env,check=True,capture_output=True)
                subprocess.run(['nook','alias','set','tls-fixture','https://localhost:443'],env=env,check=True,capture_output=True)
                for trusted in [False, True]:
                    command = ['nook','tunnel','alias','tls-fixture']
                    if trusted:
                        command += ['--upstream-ca',str(ca)]
                    nook = subprocess.Popen(command, cwd=root, env=env, stdout=nook_log, stderr=nook_log)
                    alias_url = until(url_ready)
                    if trusted:
                        until(lambda: fetch(alias_url) == 'tls-fixture')
                    else:
                        # Discovery is not an upstream TLS probe. A request must fail closed.
                        def tls_rejected():
                            try:
                                urllib.request.urlopen(urllib.request.Request(alias_url, headers={'ngrok-skip-browser-warning':'1'}),timeout=10)
                            except urllib.error.HTTPError as error:
                                return error.code in (502, 503)
                            except OSError:
                                return False
                            return False
                        until(tls_rejected)
                    registry = json.loads(registry_path.read_text())
                    tunnel_id = next(iter(registry['tunnels']))
                    subprocess.run(['nook','tunnel','stop',tunnel_id],env=env,check=True,capture_output=True)
                    nook.wait(timeout=15)
                    subprocess.run(['nook','prune'],env=env,check=True,capture_output=True)
                    assert not json.loads(registry_path.read_text())['tunnels']
                    assert 'tls-fixture.localhost' in json.loads(registry_path.read_text())['aliases']
                print("PASS: ngrok HTTP, signed binary webhook, Vite WebSocket/HMR, verified/untrusted TLS aliases and cleanup")
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
