#!/usr/bin/env python3
"""Opt-in real Docker + native Caddy proof, isolated projects and state only.

NOOK_COMPOSE_E2E=1 python3 tests/compose_e2e.py
Requires docker compose, caddy, and the built target/debug/nook binary.
"""
import json
import os
from pathlib import Path
import socket
import signal
import subprocess
import tempfile
import time
import urllib.request
import uuid


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    if os.environ.get("NOOK_COMPOSE_E2E") != "1":
        print("SKIP: set NOOK_COMPOSE_E2E=1")
        return
    binary = Path(__file__).resolve().parents[1] / "target/debug" / ("nook.exe" if os.name == "nt" else "nook")
    project = "nook-compose-proof-" + uuid.uuid4().hex[:12]
    cleanup_projects = [project]
    with tempfile.TemporaryDirectory(prefix="nook compose proof ") as directory:
        root = Path(directory)
        env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"), XDG_DATA_HOME=str(root / "data"), NOOK_DISABLE_UPDATE_CHECK="1", APPDATA=str(root / "config"), LOCALAPPDATA=str(root / "state"))
        docker_caddy = os.environ.get("NOOK_TEST_CADDY_DOCKER") == "1"
        admin, http = free_port(), (free_port() if docker_caddy else 80)
        https = free_port() if docker_caddy else 443
        proxy_host = "127.92." + str(int(uuid.uuid4().hex[:2], 16)) + ".2"
        caddyfile = root / "Caddyfile"
        caddyfile.write_text(f"{{\n admin 127.0.0.1:{admin}\n auto_https disable_redirects\n skip_install_trust\n}}\nhttp://localhost:{http} {{\n bind {proxy_host}\n respond foreign\n}}\nhttps://localhost:{https} {{\n bind {proxy_host}\n tls internal\n respond foreign-tls\n}}\n")
        if docker_caddy:
            proxy_host = "127.0.0.1"
            caddyfile.write_text("{\n admin 0.0.0.0:2019\n auto_https disable_redirects\n skip_install_trust\n}\nhttp://localhost:80 {\n respond foreign\n}\nhttps://localhost:443 {\n tls internal\n respond foreign-tls\n}\n")
        log = (root / "caddy.log").open("w+")
        caddy_args = [os.environ.get("NOOK_TEST_CADDY", "caddy"), "run", "--config", str(caddyfile), "--adapter", "caddyfile"]
        docker_caddy = os.environ.get("NOOK_TEST_CADDY_DOCKER") == "1"
        if docker_caddy:
            caddy_args = ["docker", "run", "--rm", "--name", project + "-caddy", "--add-host", "host.docker.internal:host-gateway", "-p", f"127.0.0.1:{admin}:2019", "-p", f"127.0.0.1:{http}:80", "-p", f"127.0.0.1:{https}:443", "-v", str(caddyfile) + ":/etc/caddy/Caddyfile:ro", "caddy:2.11.9-alpine", "caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]
        if docker_caddy and os.environ.get("NOOK_TEST_CADDY_PROXY") == "1":
            image_index = caddy_args.index("caddy:2.11.9-alpine")
            caddy_args[image_index:] = ["-v", "/var/run/docker.sock:/var/run/docker.sock:ro", "lucaslorentz/caddy-docker-proxy:2.13.1-alpine", "docker-proxy", "--caddyfile-path", "/etc/caddy/Caddyfile"]
        caddy = subprocess.Popen(caddy_args, env=env, stdout=log, stderr=log)

        def command(argv, success=True):
            result = subprocess.run([str(a) for a in argv], cwd=root, env=env, capture_output=True, text=True, timeout=90)
            if success and result.returncode:
                raise AssertionError(f"{argv}\n{result.stdout}\n{result.stderr}")
            return result

        def nook(*args, success=True):
            result = command([binary, *args], success)
            if os.environ.get("NOOK_TEST_CADDY_PROXY") == "1" and args[0] == "compose" and ("up" in args or "down" in args) and result.returncode == 0:
                # The documented compatibility contract is convergence after the
                # plugin reload, not uninterrupted control by two controllers.
                time.sleep(.5)
                command([binary, "prune"])
            return result

        def compose(*args, success=True):
            return nook("compose", "--no-worktree", "-p", project, *args, success=success)

        def registry():
            return json.loads((root / "state/nook/state.json").read_text())

        def route(host):
            req = urllib.request.Request(f"http://{proxy_host}:{http}/", headers={"Host": host})
            return urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=3).read().decode()

        def config():
            return json.load(urllib.request.urlopen(f"http://127.0.0.1:{admin}/config/"))

        compose_file = root / "compose.yaml"
        service = {
            "image": "caddy:2.11.9-alpine",
            "command": ["sh", "-c", "caddy respond --listen :9090 --body nook-admin & exec caddy respond --listen :8080 --body nook-http"],
            "ports": [{"target": 8080, "host_ip": ("0.0.0.0" if docker_caddy else "127.0.0.1"), "protocol": "tcp"}],
            "labels": {"io.ntnook.route.http.port": "8080", "io.ntnook.route.http.name": "web", "io.ntnook.route.http.tls": "false"},
        }
        service["ports"].append({"target": 9090, "host_ip": ("0.0.0.0" if docker_caddy else "127.0.0.1"), "protocol": "tcp"})
        service["labels"].update({"io.ntnook.route.admin.port": "9090", "io.ntnook.route.admin.name": "admin", "io.ntnook.route.admin.tls": "false"})
        service["labels"].update({"io.ntnook.route.secure.port": "8080", "io.ntnook.route.secure.name": "secure"})
        api_labels = {key: value for key, value in service["labels"].items() if ".admin." not in key and ".secure." not in key}
        model = {"services": {"web": dict(service), "api": dict(service, labels={**api_labels, "io.ntnook.route.http.name": "api"}), "db": {"image": "alpine:3.20", "command": ["sleep", "3600"], "volumes": ["data:/data"], "expose": ["5432", "6379/udp"]}}, "volumes": {"data": {}}}
        # JSON is a YAML subset and keeps test fixtures platform independent.
        compose_file.write_text(json.dumps(model))
        try:
            for _ in range(100):
                try:
                    initial = config()
                    break
                except OSError:
                    if caddy.poll() is not None:
                        raise AssertionError("Caddy failed: " + (root / "caddy.log").read_text())
                    time.sleep(.05)
            else:
                raise AssertionError("Caddy did not start")
            server = next(name for name, value in initial["apps"]["http"]["servers"].items() if any(address.endswith(":80") for address in value["listen"]))
            config_dir = root / "config" / ("Nook" if os.name == "nt" else "nook")
            config_dir.mkdir(parents=True)
            (config_dir / "config.toml").write_text(f'format_version = 1\ncaddy_admin = "http://127.0.0.1:{admin}"\nhttp_server = "{server}"\n')
            if docker_caddy:
                gateway = command(["docker", "network", "inspect", "bridge", "--format", "{{(index .IPAM.Config 0).Gateway}}"] ).stdout.strip()
                with (config_dir / "config.toml").open("a") as f:
                    f.write(f'caddy_loopback_host = "host.docker.internal"\ncaddy_client_ip_ranges = ["127.0.0.0/8", "{gateway}/32"]\n')
            before = initial["apps"]["http"]["servers"][server]["routes"]
            print("PROOF up detached, explicit HTTP only, dynamic publications", flush=True)
            compose("up", "-d")
            state = registry()
            owned = next(iter(state["compose_projects"].values()))
            assert len(owned["routes"]) == 4
            assert "nook-admin" in route(owned["routes"]["web/admin"]["alias"]["hostname"])
            web = owned["routes"]["web/http"]
            assert "nook-http" in route(web["alias"]["hostname"])
            assert route("localhost") == "foreign"
            print("PROOF HTTPS endpoint uses the existing local CA", flush=True)
            ca = root / "local-ca.pem"
            nook("ca", "export", str(ca))
            secure = owned["routes"]["web/secure"]["alias"]["hostname"]
            response = command(["curl", "--retry", "5", "--retry-all-errors", "--retry-delay", "1", "--fail", "--silent", "--show-error", "--noproxy", "*", "--cacert", ca, "--resolve", f"{secure}:{https}:{proxy_host}", f"https://{secure}:{https}/"])
            assert "nook-http" in response.stdout
            assert before[0] in config()["apps"]["http"]["servers"][server]["routes"]

            print("PROOF no force adoption through alias/run", flush=True)
            assert nook("alias", "set", web["alias"]["hostname"], "9999", "--force", "--no-tls", success=False).returncode != 0
            assert nook("run", "--name", web["alias"]["hostname"], "--force", "--no-tls", "--", "does-not-exist", success=False).returncode != 0

            print("PROOF recreate keeps route owner", flush=True)
            compose("up", "-d", "--force-recreate", "web")
            current = next(iter(registry()["compose_projects"].values()))
            assert current["routes"]["web/http"]["alias"]["id"] == web["alias"]["id"]
            assert "nook-http" in route(web["alias"]["hostname"])

            print("PROOF partial down preserves API, DB and volume", flush=True)
            compose("down", "web")
            current = next(iter(registry()["compose_projects"].values()))
            assert "web/http" not in current["routes"]
            assert "nook-http" in route(current["routes"]["api/http"]["alias"]["hostname"])
            assert command(["docker", "ps", "-q", "--filter", f"label=com.docker.compose.project={project}", "--filter", "label=com.docker.compose.service=db"]).stdout.strip()

            print("PROOF Caddy reload is recovered without changing foreign routes", flush=True)
            req = urllib.request.Request(f"http://127.0.0.1:{admin}/load", data=json.dumps(initial).encode(), headers={"Content-Type": "application/json"}, method="POST")
            urllib.request.urlopen(req, timeout=3).close()
            nook("prune")
            assert "nook-http" in route(current["routes"]["api/http"]["alias"]["hostname"])
            assert route("localhost") == "foreign"

            print("PROOF stopped external service loses route on prune", flush=True)
            command(["docker", "compose", "-p", project, "stop", "api"])
            nook("prune")
            current = next(iter(registry()["compose_projects"].values()))
            assert not current["routes"]["api/http"]["active"]
            compose("up", "-d", "api")

            print("PROOF foreign project container blocks up/down", flush=True)
            foreign = command(["docker", "run", "-d", "--label", f"com.docker.compose.project={project}", "--label", "com.docker.compose.service=intruder", "alpine:3.20", "sleep", "3600"]).stdout.strip()
            try:
                assert compose("up", "-d", success=False).returncode != 0
                assert compose("down", success=False).returncode != 0
                assert command(["docker", "inspect", "--format", "{{.State.Running}}", foreign]).stdout.strip() == "true"
            finally:
                command(["docker", "rm", "-f", foreign])

            print("PROOF multi-file, env-file, profiles and partial up", flush=True)
            override = root / "compose.extra.json"
            extra = dict(service, profiles=["tools"], labels={**api_labels, "io.ntnook.route.http.name": "${NOOK_ROUTE_NAME}"})
            override.write_text(json.dumps({"services": {"debug": extra}}))
            env_file = root / "local env"
            env_file.write_text("NOOK_ROUTE_NAME=debug\n")
            nook("compose", "-f", "compose.yaml", "-f", "compose.extra.json", "--env-file", "local env", "--profile", "tools", "--no-worktree", "-p", project, "up", "-d", "debug")
            current = next(iter(registry()["compose_projects"].values()))
            assert "debug/http" in current["routes"]
            assert "nook-http" in route(current["routes"]["debug/http"]["alias"]["hostname"])
            compose("down", "debug")

            print("PROOF fixed port collision fails without changing another container", flush=True)
            with socket.socket() as occupied:
                occupied.bind(("0.0.0.0", 0))
                occupied.listen()
                collision = dict(service, labels={**api_labels, "io.ntnook.route.http.name": "collision"}, ports=[{"target": 8080, "published": str(occupied.getsockname()[1]), "host_ip": "0.0.0.0"}])
                override.write_text(json.dumps({"services": {"collision": collision}}))
                result = nook("compose", "-f", "compose.yaml", "-f", "compose.extra.json", "--no-worktree", "-p", project, "up", "-d", "collision", success=False)
                assert result.returncode != 0
            compose("down", "collision")

            print("PROOF attached Ctrl+C stops only its selected service", flush=True)
            attached_log = (root / "attached.log").open("w+")
            attached = subprocess.Popen([str(binary), "compose", "--no-worktree", "-p", project, "up", "web"], cwd=root, env=env, stdout=attached_log, stderr=attached_log, creationflags=(subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0))
            try:
                for _ in range(150):
                    current = next(iter(registry()["compose_projects"].values()))
                    if current["routes"].get("web/http", {}).get("active"):
                        break
                    if attached.poll() is not None:
                        raise AssertionError("attached up failed: " + (root / "attached.log").read_text())
                    time.sleep(.1)
                else:
                    raise AssertionError("attached service did not become ready")
                attached.send_signal(signal.CTRL_BREAK_EVENT if os.name == "nt" else signal.SIGINT)
                assert attached.wait(timeout=25) == 130
                current = next(iter(registry()["compose_projects"].values()))
                assert not current["routes"]["web/http"]["active"]
                assert current["routes"]["api/http"]["active"]
            finally:
                if attached.poll() is None:
                    attached.kill()
                    attached.wait()
                attached_log.close()

            if os.name != "nt":
                print("PROOF supervisor crash leaves recoverable persistent ownership", flush=True)
                with (root / "crash.log").open("w+") as crash_log:
                    crashed = subprocess.Popen([str(binary), "compose", "--no-worktree", "-p", project, "up", "web"], cwd=root, env=env, stdout=crash_log, stderr=crash_log)
                    try:
                        for _ in range(150):
                            current = next(iter(registry()["compose_projects"].values()))
                            if current["routes"].get("web/http", {}).get("active"):
                                break
                            if crashed.poll() is not None:
                                raise AssertionError("crash fixture exited early")
                            time.sleep(.1)
                        else:
                            raise AssertionError("crash fixture did not start")
                        recorded_id = current["routes"]["web/http"]["alias"]["id"]
                        crashed.kill()
                        crashed.wait()
                        # Linux's existing managed-child parent-death signal asks Compose
                        # to stop its attached services. Recover based on Docker, not PID.
                        for _ in range(100):
                            running = command(["docker", "ps", "-q", "--filter", f"label=com.docker.compose.project={project}", "--filter", "label=com.docker.compose.service=web"]).stdout.strip()
                            if not running:
                                break
                            time.sleep(.1)
                        assert not running
                        req = urllib.request.Request(f"http://127.0.0.1:{admin}/load", data=json.dumps(initial).encode(), headers={"Content-Type": "application/json"}, method="POST")
                        urllib.request.urlopen(req, timeout=3).close()
                        nook("prune")
                        current = next(iter(registry()["compose_projects"].values()))
                        assert not current["routes"]["web/http"]["active"]
                        compose("up", "-d", "web")
                        current = next(iter(registry()["compose_projects"].values()))
                        assert current["routes"]["web/http"]["alias"]["id"] == recorded_id
                        assert "nook-http" in route(current["routes"]["web/http"]["alias"]["hostname"])
                    finally:
                        if crashed.poll() is None:
                            crashed.kill()
                            crashed.wait()
                        compose("down", "web")

            print("PROOF two Git worktrees have distinct projects, owners and domains", flush=True)
            repository = root / "repository"
            repository.mkdir()
            (repository / "compose.yaml").write_text(json.dumps({"name": "worktree-proof", "services": {"web": service}}))
            command(["git", "init", "-b", "main", repository])
            command(["git", "-C", repository, "add", "compose.yaml"])
            command(["git", "-C", repository, "-c", "user.name=Nook Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "commit", "-m", "test fixture"])
            sibling = root / "sibling"
            command(["git", "-C", repository, "worktree", "add", "--detach", sibling, "HEAD"])
            identities = []
            for tree in [repository, sibling]:
                # Record the auto-derived project even if startup subsequently fails.
                result = nook("compose", "-f", str(tree / "compose.yaml"), "up", "-d", success=False)
                registered = [p for p in registry()["compose_projects"].values() if Path(p["root"]) == tree.resolve()]
                cleanup_projects.extend(p["name"] for p in registered)
                assert result.returncode == 0, result.stderr
                assert len(registered) == 1
                identities.append(registered[0])
                assert "nook-http" in route(registered[0]["routes"]["web/http"]["alias"]["hostname"])
            assert identities[0]["name"] != identities[1]["name"]
            assert identities[0]["id"] != identities[1]["id"]
            assert identities[0]["routes"]["web/http"]["alias"]["hostname"] != identities[1]["routes"]["web/http"]["alias"]["hostname"]
            for tree, registered in zip([repository, sibling], identities):
                nook("compose", "--project-directory", str(tree), "-p", registered["name"], "down")

            print("PROOF down works without original files and retains volumes", flush=True)
            compose_file.unlink()
            # An unavailable Admin API must not keep application containers running.
            saved_config = (config_dir / "config.toml").read_text()
            (config_dir / "config.toml").write_text(saved_config.replace(f":{admin}", f":{free_port()}"))
            assert compose("down", success=False).returncode != 0
            assert not command(["docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={project}"]).stdout.strip()
            (config_dir / "config.toml").write_text(saved_config)
            nook("prune")
            compose("down")
            compose("down")
            assert not command(["docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={project}"]).stdout.strip()
            assert command(["docker", "volume", "inspect", f"{project}_data"]).returncode == 0
            assert route("localhost") == "foreign"
            print("PASS Compose E2E", flush=True)
        finally:
            # Only resources of this randomly named test project; never global prune.
            for owned_project in cleanup_projects:
                ids = command(["docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={owned_project}"], False).stdout.split()
                if ids:
                    command(["docker", "rm", "-f", *ids], False)
                networks = command(["docker", "network", "ls", "-q", "--filter", f"label=com.docker.compose.project={owned_project}"], False).stdout.split()
                if networks:
                    command(["docker", "network", "rm", *networks], False)
            command(["docker", "volume", "rm", f"{project}_data"], False)
            if docker_caddy:
                command(["docker", "rm", "-f", project + "-caddy"], False)
            caddy.terminate()
            try:
                caddy.wait(timeout=5)
            except subprocess.TimeoutExpired:
                caddy.kill()
                caddy.wait()
            log.close()


if __name__ == "__main__":
    main()
