// Explicit integration gate: build nook, install the pinned test runners, then run:
// npm install --prefix target/workspace-runner-tools --no-audit --no-fund turbo@2.11.10 lerna@10.1.1 pnpm@12.15.0 bun@1.4.4 vite-plus@1.4.0 nx@24.0.0 @moonrepo/cli@2.6.2 @yarnpkg/cli-dist@4.18.1
// node --test tests/workspace_runners.mjs
// Requires Node 24.15+, Caddy 2.11.x (no CA installation).
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
const runners = [
  { id: "turbo", command: "turbo run dev", filter: ["turbo", "run", "dev", "--filter=@fixture/api"] },
  { id: "lerna", command: "lerna run dev --stream --concurrency=8", filter: ["lerna", "run", "dev", "--scope=@fixture/api", "--stream"] },
  { id: "pnpm", command: "pnpm -r --parallel --filter '@fixture/*' run dev", filter: ["pnpm", "--filter", "@fixture/api", "run", "dev"] },
  { id: "bun", command: "bun run --shell=system --parallel --filter '@fixture/*' dev", filter: ["bun", "run", "--shell=system", "--filter", "@fixture/api", "dev"] },
  { id: "bun-runtime", command: "bun run --bun --shell=bun --parallel --filter '@fixture/*' dev", filter: ["bun", "run", "--bun", "--shell=bun", "--filter", "@fixture/api", "dev"] },
  { id: "vite-task", command: "vp run --parallel --no-cache --filter '@fixture/*' dev", filter: ["vp", "run", "--no-cache", "--filter", "@fixture/api", "dev"] },
  { id: "vite-dev", command: "vp run --parallel --no-cache --filter '@fixture/*' dev", filter: ["vp", "run", "--no-cache", "--filter", "@fixture/api", "dev"] },
  { id: "nx", command: "nx run-many -t dev --parallel=8 --skipNxCache --outputStyle=stream", filter: ["nx", "run-many", "-t", "dev", "-p", "@fixture/api", "--skipNxCache", "--outputStyle=stream"] },
  { id: "moon", command: "moon run :dev", filter: ["moon", "run", "api:dev"] },
  { id: "yarn", command: "yarn workspaces foreach -A -p -i -j unlimited --include '@fixture/*' run dev", filter: ["yarn", "workspaces", "foreach", "-A", "-p", "-i", "--include", "@fixture/api", "run", "dev"] },
];
const selectedRunners = process.env.NOOK_TEST_RUNNERS?.split(",");
if (selectedRunners) {
  assert.ok(selectedRunners.every((id) => runners.some((runner) => runner.id === id)), `Unknown NOOK_TEST_RUNNERS selection: ${process.env.NOOK_TEST_RUNNERS}`);
}

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
      if (error.fatal) throw error;
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
function adminConfig(url, host) {
  return new Promise((resolve, reject) => {
    const request = get(url, { headers: host ? { host } : {} }, (response) => {
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

const serverInfoSource = `
import fs from 'node:fs';
import path from 'node:path';
const id = path.basename(process.cwd());
const info = {id, pid:process.pid, cwd:process.cwd(), port:process.env.PORT,
  host:process.env.HOST, url:process.env.NOOK_URL, api:process.env.API_UPSTREAM,
  label:process.env.APP_LABEL, dotenv:process.env.NOOK_TEST_DOTENV ?? null, bun:process.versions.bun ?? null,
  session:process.env.NOOK_WORKSPACE_SESSION ?? null, recovery:process.env.NOOK_WORKSPACE_PROCESS ?? null};
`;
const serverSource = `
import http from 'node:http';
${serverInfoSource}
const server = http.createServer((req,res) => res.end(JSON.stringify(info)));
server.listen(Number(info.port), info.host, () => {
  fs.writeFileSync(path.join(process.cwd(), 'started.json'), JSON.stringify(info));
  console.log('started ' + id);
  if (process.env.NOOK_TEST_EXIT_AFTER) setTimeout(() => server.close(() => process.exit(0)), Number(process.env.NOOK_TEST_EXIT_AFTER));
});
process.on('SIGTERM', () => server.close(() => process.exit(0)));
setInterval(() => { if(fs.existsSync('fail-server')) process.exit(7); }, 50).unref();
`;

for (const { id: runner, command, filter } of runners.filter(({ id }) => !selectedRunners || selectedRunners.includes(id))) {
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
          routes: [{ match: [{ host: ["fallback.runner.localhost"] }], handle: [{ handler: "static_response", body: "fallback" }] }],
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
        NX_ISOLATE_PLUGINS: "false",
        COREPACK_ENABLE_PROJECT_SPEC: "0",
        PATH: [path.dirname(nook), path.join(tools, "node_modules/.bin"), process.env.PATH].join(path.delimiter),
      };
      delete environment.NOOK_WORKSPACE_SESSION;
      delete environment.NOOK_TEST_DOTENV;
      caddy = launch("caddy", ["run", "--config", path.join(root, "caddy.json")], { cwd: root, env: environment });
      await waitFor(() => adminConfig(`${admin}/config/`), "Caddy readiness");
      await writeFile(path.join(root, "config/nook/config.toml"), `format_version = 1\ncaddy_admin = "${admin}"\n`);
      await writeFile(path.join(root, "package.json"), JSON.stringify({
        name: "nook-runner-fixture", private: true, version: "1.0.0",
        packageManager: runner === "yarn" ? "yarn@4.18.1" : runner === "pnpm" ? "pnpm@12.15.0" : "npm@11.19.1", workspaces: ["apps/*", "wrapper"],
        scripts: { dev: `node root-probe.mjs && ${command}` },
      }));
      await writeFile(path.join(root, "pnpm-workspace.yaml"), `packages:\n  - apps/*\n  - wrapper\npmOnFail: ignore\nincludeWorkspaceRoot: false\nlinkWorkspacePackages: true\nstoreDir: ${JSON.stringify(path.join(root, "pnpm-store"))}\n`);
      await mkdir(path.join(root, "wrapper"));
      await writeFile(path.join(root, "wrapper/package.json"), JSON.stringify({ name: "@neotamia/nook-run", version: "1.0.1", bin: { "nook-run": "bin/nook-run.js" } }));
      await symlink(path.join(repository, "packages/nook-run/bin"), path.join(root, "wrapper/bin"), "dir");
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
      // This executes inside the root command, before the real runner starts.
      await writeFile(path.join(root, "root-probe.mjs"), `
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
assert.ok(process.env.NOOK_WORKSPACE_SESSION);
assert.equal(Object.keys(JSON.parse(fs.readFileSync(${JSON.stringify(path.join(root, "state/nook/state.json"))})).leases).length, 2);
const config = await new Promise((resolve, reject) => http.get(${JSON.stringify(`${admin}/config/`)}, res => {
  let body=''; res.on('data',chunk=>body+=chunk); res.on('end',()=>resolve(body));
}).on('error',reject));
for (const id of ['api','web']) assert.ok(config.includes(id+'.runner.localhost'));
fs.appendFileSync('root-starts', 'start\\n');
`);
      await writeFile(path.join(root, "claim-probe.mjs"), `
import assert from 'node:assert/strict';
import fs from 'node:fs';
assert.ok(process.env.NOOK_WORKSPACE_SESSION, 'runner must forward session to wrapper');
fs.appendFileSync('claims', 'claim\\n');
`);
      await writeFile(path.join(root, "watcher.mjs"), `
import fs from 'node:fs';
fs.writeFileSync('watcher-ran',String(process.pid));
process.on('SIGTERM', () => process.exit(0));
setInterval(()=>{},1000);
`);
      await mkdir(path.join(root, "apps"));
      for (const id of ["api", "web", "watcher"]) {
        const dir = path.join(root, "apps", id);
        await mkdir(dir);
        await writeFile(path.join(dir, "package.json"), JSON.stringify({
          name: `@fixture/${id}`, version: "1.0.0", private: true,
          ...(id === "web" ? { dependencies: { "@fixture/api": "1.0.0" } } : {}),
          ...(id !== "watcher" ? { devDependencies: { "@neotamia/nook-run": "1.0.1" } } : {}),
          nx: { targets: { dev: { cache: false, continuous: true, ...(id === "web" ? { dependsOn: ["^dev"] } : {}) } } },
          scripts: { dev: id === "watcher"
            ? "node ../../watcher.mjs"
            : `node ../../claim-probe.mjs && nook-run -- ${runner === "vite-dev" ? "vp dev" : "node ../../server.mjs"}` },
        }));
        await mkdir(path.join(dir, "node_modules/.bin"), { recursive: true });
        await symlink(wrapper, path.join(dir, "node_modules/.bin/nook-run"));
        if (runner === "vite-dev" && id !== "watcher") {
          await writeFile(path.join(dir, "vite.config.mjs"), `
${serverInfoSource}
export default {
  server: {host:info.host, port:Number(info.port), strictPort:true},
  plugins: [{name:'nook-runner-fixture', configureServer(server) {
    server.middlewares.use((req,res) => res.end(JSON.stringify(info)));
    server.httpServer.once('listening', () => fs.writeFileSync('started.json',JSON.stringify(info)));
    setInterval(() => { if(fs.existsSync('fail-server')) process.exit(7); }, 50).unref();
  }}]
};
`);
        }
        if (runner === "lerna" && id !== "watcher") {
          await writeFile(path.join(dir, ".env"), `NOOK_TEST_DOTENV=${id}-dotenv\nAPP_LABEL=dotenv-must-not-override-nook\nPORT=1\nHOST=invalid.example\nNOOK_URL=http://invalid.example\n`);
        }
        if (id !== "watcher") {
          await writeFile(path.join(dir, "nook.toml"), `format_version = 1\nname = "${id}.runner"\ntls = false\n`);
        }
      }
      if (runner === "moon") {
        await mkdir(path.join(root, ".moon"));
        await writeFile(path.join(root, ".moon/workspace.yml"), "projects:\n  api: apps/api\n  web: apps/web\n  watcher: apps/watcher\ntelemetry: false\npipeline:\n  installDependencies: false\nvcs:\n  client: git\n  defaultBranch: main\n");
        for (const id of ["api", "web", "watcher"]) {
          await writeFile(path.join(root, "apps", id, "moon.yml"), "tasks:\n  dev:\n    command: npm run dev\n    toolchains: system\n    options:\n      cache: false\n      persistent: true\n      runInCI: true\n      outputStyle: stream\n");
        }
      }
      await writeFile(path.join(root, "nook.toml"), `
format_version = 2
name = "runner"
[[apps]]
id = "api"
dir = "apps/api"
[apps.env]
APP_LABEL = "api-env"
[[apps]]
id = "web"
dir = "apps/web"
[apps.env]
APP_LABEL = "web-env"
API_UPSTREAM = "\${api.upstream}"
`);
      // No tool installation is performed by this gate. Use the pinned tools above.
      const lock = spawnSync("npm", ["install", "--package-lock-only", "--ignore-scripts", "--offline", "--no-audit", "--no-fund"], { cwd: root, env: environment, encoding: "utf8" });
      assert.equal(lock.status, 0, lock.stdout + lock.stderr);
      if (runner === "yarn") {
        await writeFile(path.join(root, ".yarnrc.yml"), "nodeLinker: node-modules\nenableGlobalCache: false\nenableNetwork: false\nenableTelemetry: false\nenableScripts: false\n");
        const install = spawnSync(path.join(tools, "node_modules/.bin/yarn"), ["install", "--mode=skip-build"], { cwd: root, env: { ...environment, YARN_ENABLE_IMMUTABLE_INSTALLS: "false" }, encoding: "utf8" });
        assert.equal(install.status, 0, install.stdout + install.stderr);
      } else if (runner === "pnpm") {
        const install = spawnSync(path.join(tools, "node_modules/.bin/pnpm"), ["install", "--offline", "--ignore-scripts"], { cwd: root, env: environment, encoding: "utf8" });
        assert.equal(install.status, 0, install.stdout + install.stderr);
      } else {
        await symlink(path.join(tools, "node_modules"), path.join(root, "node_modules"), "dir");
      }
      const git = spawnSync("git", ["init", "--quiet", "--initial-branch=main"], { cwd: root, encoding: "utf8" });
      assert.equal(git.status, 0, git.stderr);
      if (runner === "moon") {
        // moon requires HEAD even for uncached system tasks. This empty commit
        // exists only in the disposable fixture repository, never this checkout.
        const commit = spawnSync("git", ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "commit", "--allow-empty", "--quiet", "-m", "fixture"], { cwd: root, encoding: "utf8" });
        assert.equal(commit.status, 0, commit.stderr);
        // Keep CI enabled, but compare fixture revisions instead of the host
        // repository's pull-request branches or commit SHAs.
        environment.MOON_BASE = "main";
        environment.MOON_HEAD = "HEAD";
      }
      running = launch(nook, ["run", "--runner", "--", "npm", "run", "dev"], { cwd: root, env: environment });
      const infos = await waitFor(async () => {
        if (running.child.exitCode !== null) throw Object.assign(new Error(running.log()), { fatal: true });
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
      assert.equal(await readFile(path.join(root, "root-starts"), "utf8"), "start\n", "root command starts once");
      for (const info of infos) {
        assert.equal(info.cwd, path.join(root, "apps", info.id));
        assert.equal(info.label, `${info.id}-env`);
        if (runner === "lerna") assert.equal(info.dotenv, `${info.id}-dotenv`, "Lerna loads each package's .env while Nook retains its resolved environment");
        assert.equal(await readFile(path.join(info.cwd, "claims"), "utf8"), "claim\n", "one wrapper invocation per selected app");
        assert.equal(info.bun !== null, runner === "bun-runtime", "Bun --bun controls the actual server runtime");
      }
      assert.equal((await (await fetch(web.api)).json()).id, "api");
      for (const info of infos) {
        assert.equal((await adminConfig("http://127.0.0.1", `${info.id}.runner.localhost`)).id, info.id);
      }
      const watcherPid = Number(await waitFor(() => readFile(path.join(root, "apps/watcher/watcher-ran"), "utf8"), "non-HTTP watcher"));
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
      running = launch(nook, ["run", "--runner", "--", ...filter], { cwd: root, env: environment });
      await waitFor(async () => JSON.parse(await readFile(path.join(root, "apps/api/started.json"), "utf8")), "filtered API task");
      await assert.rejects(readFile(path.join(root, "apps/web/started.json")), { code: "ENOENT" });
      await running.stop();
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).leases).length, 0);

      // A real package failure must reach the root, clean routes, and stop peers.
      for (const id of ["api", "web"]) await rm(path.join(root, "apps", id, "started.json"), { force: true });
      await rm(path.join(root, "apps/watcher/watcher-ran"));
      const runnerExitProbe = path.join(root, "runner-exit-probe.mjs");
      await writeFile(runnerExitProbe, `
import { spawn } from 'node:child_process';
import fs from 'node:fs';
const runner = spawn('npm', ['run', 'dev'], {stdio:'inherit'});
runner.on('exit', (code, signal) => {
  fs.writeFileSync('runner-exit.json', JSON.stringify({code, signal}));
  process.exitCode = code ?? 1;
});
`);
      running = launch(nook, ["run", "--runner", "--", process.execPath, runnerExitProbe], { cwd: root, env: environment });
      const failedInfos = await waitFor(() => Promise.all(["api", "web"].map(async (id) => JSON.parse(await readFile(path.join(root, "apps", id, "started.json"), "utf8")))), "apps before package error");
      const failedWatcherPid = Number(await waitFor(() => readFile(path.join(root, "apps/watcher/watcher-ran"), "utf8"), "watcher before package error"));
      await writeFile(path.join(root, "apps/api/fail-server"), "fail");
      if (["yarn", "lerna", "nx"].includes(runner)) {
        // These schedulers wait for continuous siblings after an error. Preserve that
        // scheduler policy, then let the runner report its nonzero result.
        await waitFor(async () => {
          try { return /\) [ZX] /.test(await readFile(`/proc/${failedInfos[0].pid}/stat`, "utf8")); }
          catch (error) { if (error.code === "ENOENT") return true; throw error; }
        }, "failed member exits");
        if (["lerna", "nx"].includes(runner)) {
          // Server exit precedes npm/wrapper completion. Nx must observe the
          // failure while its dependent is still alive, or it may mark it fulfilled.
          await waitFor(() => /Task "@fixture\/api:dev" is continuous but exited with code [1-9]\d*/.test(running.log()), "Nx observes failed API task");
        }
        assert.equal(running.child.exitCode, null, "runner waits for other persistent tasks");
        assert.equal((await adminConfig("http://127.0.0.1", "web.runner.localhost")).pid, failedInfos[1].pid, "sibling still serves HTTP after member failure");
        process.kill(failedInfos[1].pid, "SIGTERM");
        process.kill(failedWatcherPid, "SIGTERM");
      }
      await waitFor(() => running.child.exitCode !== null, "package failure reaches workspace root");
      const runnerExit = JSON.parse(await readFile(path.join(root, "runner-exit.json"), "utf8"));
      assert.equal(runnerExit.signal, null, running.log());
      assert.equal(running.child.exitCode, runnerExit.code, "Nook preserves the runner's exit code");
      assert.notEqual(running.child.exitCode, 0, running.log());
      await rm(path.join(root, "apps/api/fail-server"));
      for (const { pid } of [...failedInfos, { pid: failedWatcherPid }]) {
        await waitFor(async () => {
          try { return /\) [ZX] /.test(await readFile(`/proc/${pid}/stat`, "utf8")); }
          catch (error) { if (error.code === "ENOENT") return true; throw error; }
        }, "package failure stops siblings");
      }
      const afterFailure = JSON.parse(await readFile(registryPath, "utf8"));
      assert.equal(Object.keys(afterFailure.leases).length, 0);
      assert.equal(Object.keys(afterFailure.workspace_groups ?? {}).length, 0, running.log());
      const failureConfig = JSON.stringify(await adminConfig(`${admin}/config/`));
      assert.ok(!failureConfig.includes("api.runner.localhost") && !failureConfig.includes("web.runner.localhost"));
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).workspace_groups ?? {}).length, 0);

      // Recover after SIGKILL with the real runner and its non-HTTP task alive.
      for (const id of ["api", "web"]) await rm(path.join(root, "apps", id, "started.json"), { force: true });
      await rm(path.join(root, "apps/watcher/watcher-ran"));
      running = launch(nook, ["run", "--runner", "--", "npm", "run", "dev"], { cwd: root, env: environment });
      const realCrashInfos = await waitFor(() => Promise.all(["api", "web"].map(async (id) => JSON.parse(await readFile(path.join(root, "apps", id, "started.json"), "utf8")))), "real runner apps before crash");
      const realWatcherPid = Number(await waitFor(() => readFile(path.join(root, "apps/watcher/watcher-ran"), "utf8"), "real runner watcher before crash"));
      running.child.kill("SIGKILL");
      await running.ended;
      const realPrune = spawnSync(nook, ["prune"], { cwd: root, env: environment, encoding: "utf8" });
      assert.equal(realPrune.status, 0, realPrune.stdout + realPrune.stderr);
      for (const { pid } of [...realCrashInfos, { pid: realWatcherPid }]) {
        await waitFor(async () => {
          try { return /\) [ZX] /.test(await readFile(`/proc/${pid}/stat`, "utf8")); }
          catch (error) { if (error.code === "ENOENT") return true; throw error; }
        }, "real runner child removed by prune");
      }
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).leases).length, 0, realPrune.stdout + realPrune.stderr);
      assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).workspace_groups ?? {}).length, 0, realPrune.stdout + realPrune.stderr);
      const prunedConfig = JSON.stringify(await adminConfig(`${admin}/config/`));
      assert.ok(!prunedConfig.includes("api.runner.localhost") && !prunedConfig.includes("web.runner.localhost"));

      if (runner === "turbo") {
        // The same version-1 member and wrapper still work outside a session.
        await rm(path.join(root, "apps/api/started.json"));
        running = launch(process.execPath, [wrapper, "--", process.execPath, path.join(root, "server.mjs")], { cwd: path.join(root, "apps/api"), env: environment });
        const standalone = await waitFor(async () => JSON.parse(await readFile(path.join(root, "apps/api/started.json"), "utf8")), "standalone member wrapper");
        assert.equal(standalone.session, null);
        assert.equal(standalone.url, "http://api.runner.localhost");
        assert.equal((await adminConfig("http://127.0.0.1", "api.runner.localhost")).pid, standalone.pid);
        assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).workspace_groups ?? {}).length, 0);
        await running.stop();
        assert.equal(Object.keys(JSON.parse(await readFile(registryPath, "utf8")).leases).length, 0);
      }

      // These generic session-contract regressions do not depend on a runner's
      // scheduler. Retain the original Turbo/Lerna coverage without rerunning
      // the synthetic scheduler for every additional tool.
      if (!["turbo", "lerna"].includes(runner)) return;

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
      error.message += `\nRunner output:\n${running?.log() ?? "not started"}\nCaddy output:\n${caddy?.log().slice(-3_000) ?? "not started"}`;
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
