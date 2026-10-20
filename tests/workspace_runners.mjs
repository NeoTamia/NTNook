// Explicit integration gate: build nook, install the pinned test runners, then run:
// npm install --prefix target/workspace-runner-tools --no-audit --no-fund turbo@2.11.10 lerna@10.1.1
// node --test tests/workspace_runners.mjs
// Requires Node 22+, Caddy 2.11.x and a free loopback port 80 (no CA installation).
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, mkdir, readFile, readlink, writeFile, rm, symlink } from "node:fs/promises";
import { createServer } from "node:net";
import { get } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const tools = path.resolve(process.env.NOOK_RUNNER_TOOLS ?? path.join(repository, "target/workspace-runner-tools"));
const nook = path.resolve(process.env.NOOK_TEST_BINARY ?? path.join(repository, "target/debug/nook"));
const wrapper = path.join(repository, "packages/nook-run/bin/nook-run.js");

// Local network namespaces run as root. Use ordinary-user process inspection
// permissions for both the crash fixture and its recovering prune invocation.
const withoutCapabilities = `
import ctypes,os,sys
libc=ctypes.CDLL(None,use_errno=True)
assert libc.prctl(38,1,0,0,0)==0
header=(ctypes.c_uint32*2)(0x20080522,0)
data=(ctypes.c_uint32*6)()
assert libc.capset(ctypes.byref(header),ctypes.byref(data))==0
if sys.argv[1]!='-':
    try:
        os.readlink('/proc/'+sys.argv[1]+'/ns/user')
    except PermissionError:
        pass
    else:
        raise AssertionError('protected process must deny inspection')
os.execv(sys.argv[2],sys.argv[2:])
`;

// Match the Rust integration harness: local runs get a private loopback network
// and namespace-local root, so port 80 needs no host privileges or free listener.
if (process.platform === "linux" && !process.env.NOOK_RUNNER_DIRECT) {
  if (!process.env.NOOK_RUNNER_NETNS) {
    const namespaceEnv = { ...process.env, NOOK_RUNNER_NETNS: "1" };
    delete namespaceEnv.NODE_TEST_CONTEXT;
    const result = spawnSync("unshare", ["--user", "--map-root-user", "--net", process.execPath, "--test", fileURLToPath(import.meta.url)], {
      env: namespaceEnv,
      stdio: "inherit",
    });
    if (result.error) throw result.error;
    process.exit(result.status ?? 1);
  }
  const result = spawnSync("ip", ["link", "set", "lo", "up"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
}

async function waitFor(check, description, budget = 30_000) {
  const deadline = Date.now() + budget;
  let last;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      last = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`Timed out: ${description}${last ? `: ${last.message}` : ""}`);
}

function launch(executable, args, options) {
  const child = spawn(executable, args, { stdio: ["ignore", "pipe", "pipe"], ...options });
  let log = "";
  child.stdout.on("data", (chunk) => { log = (log + chunk).slice(-32_000); });
  child.stderr.on("data", (chunk) => { log = (log + chunk).slice(-32_000); });
  const ended = once(child, "exit");
  return {
    child,
    ended,
    log: () => log,
    async stop() {
      if (child.exitCode !== null || child.signalCode !== null) return;
      child.kill("SIGTERM");
      const timer = setTimeout(() => child.kill("SIGKILL"), 8_000);
      try { await ended; } finally { clearTimeout(timer); }
    },
  };
}

async function availablePort() {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

// Node fetch adds browser-style Sec-Fetch headers, which Caddy's Admin API
// deliberately rejects without an Origin. Use an ordinary HTTP client here.
function adminConfig(url) {
  return new Promise((resolve, reject) => {
    const request = get(url, (response) => {
      let body = "";
      response.setEncoding("utf8");
      response.on("data", (chunk) => { body += chunk; });
      response.on("end", () => {
        try {
          assert.equal(response.statusCode, 200, body);
          resolve(JSON.parse(body));
        } catch (error) { reject(error); }
      });
    });
    request.setTimeout(2_000, () => request.destroy(new Error("Admin request timed out")));
    request.on("error", reject);
  });
}

const serverSource = `
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
const id = path.basename(process.cwd());
const info = {id, pid:process.pid, cwd:process.cwd(), port:process.env.PORT,
  host:process.env.HOST, url:process.env.NOOK_URL, api:process.env.API_UPSTREAM,
  session:process.env.NOOK_WORKSPACE_SESSION ?? null, recovery:process.env.NOOK_WORKSPACE_PROCESS ?? null};
const server = http.createServer((req,res) => res.end(JSON.stringify(info)));
server.listen(Number(info.port), info.host, () => {
  fs.writeFileSync(path.join(process.cwd(), 'started.json'), JSON.stringify(info));
  console.log('started ' + id);
  if (process.env.NOOK_TEST_EXIT_AFTER) setTimeout(() => server.close(() => process.exit(0)), Number(process.env.NOOK_TEST_EXIT_AFTER));
});
process.on('SIGTERM', () => server.close(() => process.exit(0)));
`;

for (const runner of ["turbo", "lerna"]) {
  test(`${runner}: existing nook-run scripts join the workspace`, { timeout: 120_000 }, async () => {
    const root = await mkdtemp(path.join(tmpdir(), `nook-${runner}-`));
    let caddy;
    let running;
    let protectedProcess;
    try {
      await mkdir(path.join(root, "config/nook"), { recursive: true });
      const adminPort = await availablePort();
      const admin = `http://127.0.0.1:${adminPort}`;
      await writeFile(path.join(root, "caddy.json"), JSON.stringify({
        admin: { listen: `127.0.0.1:${adminPort}` },
        apps: { http: { servers: { workspace: {
          listen: ["127.0.0.1:80"],
          routes: [{ handle: [{ handler: "static_response", body: "fallback" }] }],
        } } } },
      }));
      const environment = {
        ...process.env,
        XDG_CONFIG_HOME: path.join(root, "config"),
        XDG_STATE_HOME: path.join(root, "state"),
        XDG_DATA_HOME: path.join(root, "data"),
        NOOK_DISABLE_UPDATE_CHECK: "1",
        DO_NOT_TRACK: "1",
        TURBO_TELEMETRY_DISABLED: "1",
        NX_DAEMON: "false",
        NX_NO_CLOUD: "true",
        PATH: [path.dirname(nook), path.join(tools, "node_modules/.bin"), process.env.PATH].join(path.delimiter),
      };
      delete environment.NOOK_WORKSPACE_SESSION;
      caddy = launch("caddy", ["run", "--config", path.join(root, "caddy.json")], { cwd: root, env: environment });
      await waitFor(() => adminConfig(`${admin}/config/`), "Caddy readiness");
      await writeFile(path.join(root, "config/nook/config.toml"), `format_version = 1\ncaddy_admin = "${admin}"\n`);
      await writeFile(path.join(root, "package.json"), JSON.stringify({
        name: "nook-runner-fixture", private: true, version: "1.0.0",
        packageManager: "npm@11.19.1", workspaces: ["apps/*"],
        scripts: { dev: runner === "turbo" ? "turbo run dev" : "lerna run dev --stream --concurrency=8" },
      }));
      await writeFile(path.join(root, "turbo.json"), JSON.stringify({
        $schema: "https://turborepo.dev/schema.json",
        globalPassThroughEnv: ["NOOK_WORKSPACE_SESSION", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "NOOK_DISABLE_UPDATE_CHECK"],
        tasks: { dev: { cache: false, persistent: true } },
      }));
      await writeFile(path.join(root, "lerna.json"), JSON.stringify({ version: "1.0.0", npmClient: "npm" }));
      await writeFile(path.join(root, "nx.json"), JSON.stringify({
        targetDefaults: { dev: { cache: false, continuous: true } },
      }));
      await writeFile(path.join(root, "server.mjs"), serverSource);
      await mkdir(path.join(root, "apps"));
      for (const id of ["api", "web", "watcher"]) {
        const dir = path.join(root, "apps", id);
        await mkdir(dir);
        await writeFile(path.join(dir, "package.json"), JSON.stringify({
          name: `@fixture/${id}`, version: "1.0.0", private: true,
          scripts: { dev: id === "watcher"
            ? `node -e "require('node:fs').writeFileSync('watcher-ran',String(process.pid));setInterval(()=>{},1000)"`
            : `node ${JSON.stringify(wrapper)} -- node ${JSON.stringify(path.join(root, "server.mjs"))}` },
        }));
        if (id !== "watcher") {
          await writeFile(path.join(dir, "nook.toml"), `format_version = 1\nname = "${id}.runner"\ntls = false\n`);
        }
      }
      await writeFile(path.join(root, "nook.toml"), `
format_version = 2
name = "runner"
[[apps]]
id = "api"
dir = "apps/api"
[[apps]]
id = "web"
dir = "apps/web"
[apps.env]
API_UPSTREAM = "\${api.upstream}"
`);
      // No tool installation is performed by this gate. Use the pinned tools above.
      const lock = spawnSync("npm", ["install", "--package-lock-only", "--ignore-scripts", "--offline", "--no-audit", "--no-fund"], { cwd: root, env: environment, encoding: "utf8" });
      assert.equal(lock.status, 0, lock.stdout + lock.stderr);
      await symlink(path.join(tools, "node_modules"), path.join(root, "node_modules"), "dir");
      const git = spawnSync("git", ["init", "--quiet"], { cwd: root, encoding: "utf8" });
      assert.equal(git.status, 0, git.stderr);
      running = launch(nook, ["run", "--runner", "--", "npm", "run", "dev"], { cwd: root, env: environment });
      const infos = await waitFor(async () => {
        if (running.child.exitCode !== null) throw new Error(running.log());
        return Promise.all(["api", "web"].map(async (id) => JSON.parse(await readFile(path.join(root, "apps", id, "started.json"), "utf8"))));
      }, `${runner} starts both apps`);
      const [api, web] = infos;
      assert.notEqual(api.port, web.port, running.log());
      assert.equal(api.host, "127.0.0.1");
      assert.equal(web.host, "127.0.0.1");
      assert.equal(api.url, "http://api.runner.localhost");
      assert.equal(web.url, "http://web.runner.localhost");
      assert.equal(web.api, `http://127.0.0.1:${api.port}`);
      assert.equal(api.session, null, "application subprocesses must not inherit session claims");
      assert.equal(web.session, null);
      assert.equal((await (await fetch(web.api)).json()).id, "api");
      const watcherPid = Number(await readFile(path.join(root, "apps/watcher/watcher-ran"), "utf8"));
      assert.ok(watcherPid > 0);
      const registryPath = path.join(root, "state/nook/state.json");
      const registry = JSON.parse(await readFile(registryPath, "utf8"));
      assert.equal(Object.keys(registry.leases).length, 2, "exactly one lease per app");
      await running.stop();
      const final = JSON.parse(await readFile(registryPath, "utf8"));
      assert.equal(Object.keys(final.leases).length, 0, running.log());
      assert.equal(Object.keys(final.workspace_groups ?? {}).length, 0, running.log());
      const config = JSON.stringify(await adminConfig(`${admin}/config/`));
      assert.ok(!config.includes("api.runner.localhost") && !config.includes("web.runner.localhost"), config);
      for (const info of infos) {
        assert.throws(() => process.kill(info.pid, 0), { code: "ESRCH" });
      }

      // Use the runner's own package filter; Nook does not translate --only.
      for (const id of ["api", "web"]) await rm(path.join(root, "apps", id, "started.json"));
      const filteredCommand = runner === "turbo"
        ? ["turbo", "run", "dev", "--filter=@fixture/api"]
        : ["lerna", "run", "dev", "--scope=@fixture/api", "--stream"];
      running = launch(nook, ["run", "--runner", "--", ...filteredCommand], { cwd: root, env: environment });
      await waitFor(async () => JSON.parse(await readFile(path.join(root, "apps/api/started.json"), "utf8")), "filtered API task");
      await assert.rejects(readFile(path.join(root, "apps/web/started.json")), { code: "ENOENT" });
      await running.stop();
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).leases).length, 0);
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).workspace_groups ?? {}).length, 0);

      // A runner may restart the same package script. Reconcile between starts
      // must keep its route and port owned by the still-live workspace.
      const restartPath = path.join(root, "restart.mjs");
      await writeFile(restartPath, `
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
const ports = [];
for (let i=0; i<2; i++) {
  const result = spawnSync(process.execPath, [${JSON.stringify(wrapper)}, '--', process.execPath, ${JSON.stringify(path.join(root, "server.mjs"))}], {
    cwd:${JSON.stringify(path.join(root, "apps/api"))}, env:{...process.env, NOOK_TEST_EXIT_AFTER:'100'}, stdio:'inherit'
  });
  if (result.status !== 0) process.exit(result.status ?? 1);
  ports.push(JSON.parse(fs.readFileSync(${JSON.stringify(path.join(root, "apps/api/started.json"))})).port);
  const reconcile = spawnSync(${JSON.stringify(nook)}, ['list'], {env:process.env, stdio:'inherit'});
  if (reconcile.status !== 0) process.exit(reconcile.status ?? 1);
}
fs.writeFileSync(${JSON.stringify(path.join(root, "restarted.json"))}, JSON.stringify(ports));
`);
      running = launch(nook, ["run", "--runner", "--", process.execPath, restartPath], { cwd: root, env: environment });
      const restartExit = await running.ended;
      assert.equal(restartExit[0], 0, running.log());
      const ports = JSON.parse(await readFile(path.join(root, "restarted.json"), "utf8"));
      assert.equal(ports.length, 2);
      assert.equal(ports[0], ports[1], "restart retains its allocated port");

      // A detached server must be gone before its wrapper releases the claim.
      // Keep a sibling live throughout cleanup to catch group-wide signaling.
      const detachedPath = path.join(root, "detached.mjs");
      await writeFile(detachedPath, `
import { spawn } from 'node:child_process';
import fs from 'node:fs';
const child = spawn(process.execPath, [${JSON.stringify(path.join(root, "server.mjs"))}], {
  detached:true, stdio:'ignore', env:process.env
});
child.unref();
const deadline = Date.now()+5000;
while (!fs.existsSync('started.json')) {
  if (Date.now()>deadline) process.exit(1);
  await new Promise(resolve=>setTimeout(resolve,10));
}
`);
      const detachedRestart = path.join(root, "detached-restart.mjs");
      await writeFile(detachedRestart, `
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
const root=${JSON.stringify(root)}, wrapper=${JSON.stringify(wrapper)};
const server=${JSON.stringify(path.join(root, "server.mjs"))};
const sibling=spawn(process.execPath,[wrapper,'--',process.execPath,server], {
  cwd:root+'/apps/web',env:process.env,stdio:'inherit'
});
const siblingExit=once(sibling,'exit');
try {
  const deadline=Date.now()+5000;
  while (!fs.existsSync(root+'/apps/web/started.json')) {
    if (Date.now()>deadline) throw new Error('sibling did not start');
    await new Promise(resolve=>setTimeout(resolve,10));
  }
  const first=spawnSync(process.execPath,[wrapper,'--',process.execPath,${JSON.stringify(detachedPath)}], {
    cwd:root+'/apps/api',env:process.env,stdio:'inherit'
  });
  if (first.status!==0) throw new Error('detached member failed: '+first.status);
  const firstInfo=JSON.parse(fs.readFileSync(root+'/apps/api/started.json'));
  const stat=fs.existsSync('/proc/'+firstInfo.pid+'/stat') ? fs.readFileSync('/proc/'+firstInfo.pid+'/stat','utf8') : '';
  if (stat && !/\\) [ZX] /.test(stat)) throw new Error('detached server survived member cleanup: '+JSON.stringify(firstInfo)+' stat='+stat);
  const siblingInfo=JSON.parse(fs.readFileSync(root+'/apps/web/started.json'));
  process.kill(siblingInfo.pid,0);
  const second=spawnSync(process.execPath,[wrapper,'--',process.execPath,server], {
    cwd:root+'/apps/api',env:{...process.env,NOOK_TEST_EXIT_AFTER:'100'},stdio:'inherit'
  });
  if (second.status!==0) throw new Error('member restart failed: '+second.status);
  const secondInfo=JSON.parse(fs.readFileSync(root+'/apps/api/started.json'));
  if (firstInfo.port!==secondInfo.port) throw new Error('member port changed');
  process.kill(siblingInfo.pid,0);
  fs.writeFileSync(root+'/detached-restarted','ok');
} finally {
  sibling.kill('SIGTERM');
  await siblingExit;
}
`);
      for (const id of ["api", "web"]) await rm(path.join(root, "apps", id, "started.json"), { force: true });
      running = launch(nook, ["run", "--runner", "--", process.execPath, detachedRestart], { cwd: root, env: environment });
      assert.equal((await running.ended)[0], 0, running.log());
      assert.equal(await readFile(path.join(root, "detached-restarted"), "utf8"), "ok");

      // Abrupt loss of the root is recoverable through the durable registry.
      for (const id of ["api", "web"]) await rm(path.join(root, "apps", id, "started.json"), { force: true });
      // This runner intentionally has no signal forwarding of its own. The
      // library watcher outlives its leader unless Nook records runner ownership.
      const crashRunner = path.join(root, "crash-runner.mjs");
      await writeFile(crashRunner, `
import { spawn } from 'node:child_process';
import fs from 'node:fs';
for (const id of ['api','web']) spawn(process.execPath, [${JSON.stringify(wrapper)}, '--', process.execPath, ${JSON.stringify(path.join(root, "server.mjs"))}], {
  cwd:${JSON.stringify(root)}+'/apps/'+id, env:process.env, stdio:'inherit'
});
while (!fs.existsSync(${JSON.stringify(path.join(root, "protected-ready"))})) {
  await new Promise(resolve=>setTimeout(resolve,10));
}
spawn(process.execPath, ['-e', "require('node:fs').writeFileSync('watcher-ran',String(process.pid));setInterval(()=>{},1000)"], {
  cwd:${JSON.stringify(path.join(root, "apps/watcher"))}, env:process.env, detached:true, stdio:'ignore'
});
setInterval(()=>{},1000);
`);
      const sessionTemp = path.join(root, "session-temp");
      await mkdir(sessionTemp);
      running = launch("python3", ["-c", withoutCapabilities, "-", nook, "run", "--runner", "--", process.execPath, crashRunner], {
        cwd: root, env: { ...environment, TMPDIR: sessionTemp },
      });
      const crashInfos = await waitFor(() => Promise.all(["api", "web"].map(async (id) => JSON.parse(await readFile(path.join(root, "apps", id, "started.json"), "utf8")))), "apps before root crash");
      // This unrelated process predates the detached watcher and denies /proc
      // inspection. It must neither stop recovery nor receive a signal.
      protectedProcess = launch("python3", ["-c", `
import ctypes,pathlib,time
assert ctypes.CDLL(None,use_errno=True).prctl(4,0,0,0,0)==0
pathlib.Path(${JSON.stringify(path.join(root, "protected-ready"))}).write_text('ready')
time.sleep(120)
`], { cwd: root, env: environment });
      await waitFor(() => readFile(path.join(root, "protected-ready"), "utf8"), "protected unrelated process");
      const crashWatcherPid = await waitFor(async () => {
        const pid = Number(await readFile(path.join(root, "apps/watcher/watcher-ran"), "utf8"));
        return pid !== watcherPid && pid;
      }, "non-HTTP watcher before root crash");
      const beforeCrash = JSON.parse(await readFile(registryPath, "utf8"));
      const groupId = Object.values(beforeCrash.leases)[0].group_id;
      const contextPath = path.join(sessionTemp, `nook-session-${groupId}`, "context.json");
      await readFile(contextPath);
      running.child.kill("SIGKILL");
      await running.ended;
      const prune = spawnSync("python3", ["-c", withoutCapabilities, String(protectedProcess.child.pid), nook, "prune"], {
        cwd: root, env: environment, encoding: "utf8",
      });
      assert.equal(prune.status, 0, prune.stdout + prune.stderr);
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).leases).length, 0, prune.stdout + prune.stderr);
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).workspace_groups ?? {}).length, 0, prune.stdout + prune.stderr);
      process.kill(protectedProcess.child.pid, 0);
      await assert.rejects(readFile(contextPath), { code: "ENOENT" });
      for (const info of [...crashInfos, { pid: crashWatcherPid }]) {
        await waitFor(async () => {
          try {
            const stat = await readFile(`/proc/${info.pid}/stat`, "utf8");
            return /\) [ZX] /.test(stat);
          } catch (error) {
            if (error.code === "ENOENT") return true;
            throw error;
          }
        }, "crashed workspace child termination");
      }
    } catch (error) {
      error.message += `\nRunner output:\n${running?.log() ?? "not started"}\nCaddy output:\n${caddy?.log() ?? "not started"}`;
      throw error;
    } finally {
      await running?.stop();
      await protectedProcess?.stop();
      // A failing regression must not leave the fixture's detached processes.
      for (const id of ["watcher", "api", "web"]) {
        try {
          const content = await readFile(path.join(root, "apps", id, id === "watcher" ? "watcher-ran" : "started.json"), "utf8");
          const pid = id === "watcher" ? Number(content) : JSON.parse(content).pid;
          if (await readlink(`/proc/${pid}/cwd`) === path.join(root, "apps", id)) {
            process.kill(pid, "SIGKILL");
          }
        } catch (error) {
          if (error.code !== "ENOENT" && error.code !== "ESRCH") throw error;
        }
      }
      await caddy?.stop();
      await rm(root, { recursive: true, force: true });
    }
  });
}
