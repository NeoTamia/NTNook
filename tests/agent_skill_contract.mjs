// cargo build --locked && node --test tests/agent_skill_contract.mjs
// Real Caddy: NOOK_AGENT_E2E=1 node --test tests/agent_skill_contract.mjs
// Use an isolated Linux network namespace, or a disposable Windows CI host
// with loopback port 443 free. Never point this test at a personal Caddy.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { cp, mkdtemp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { get } from 'node:http';
import { get as getHttps } from 'node:https';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const bundle = path.join(repository, 'skills/nook');
const binary = path.resolve(process.env.NOOK_TEST_BINARY ?? path.join(repository, 'target/debug', process.platform === 'win32' ? 'nook.exe' : 'nook'));

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'nook agent skill '));
  const cleanup = [];
  t.after(async () => {
    for (const action of cleanup.reverse()) await action();
    await rm(root, { recursive: true, force: true });
  });
  const env = { ...process.env, NOOK_DISABLE_UPDATE_CHECK: '1',
    XDG_CONFIG_HOME: path.join(root, 'config'), XDG_STATE_HOME: path.join(root, 'state'),
    XDG_DATA_HOME: path.join(root, 'data'), APPDATA: path.join(root, 'roaming'),
    LOCALAPPDATA: path.join(root, 'local') };
  delete env.NOOK_WORKSPACE_SESSION;
  delete env.NOOK_TAILSCALE;
  // Explicit XDG overrides take precedence on Windows as well as Linux.
  const config = path.join(env.XDG_CONFIG_HOME, 'nook', 'config.toml');
  await mkdir(path.dirname(config), { recursive: true });
  // A deliberately unreachable local API makes config validation fail before spawn.
  await writeFile(config, 'format_version = 1\ncaddy_admin = "http://127.0.0.1:0"\n');
  const command = (args, cwd = root) => {
    const result = spawnSync(binary, args, { cwd, env, encoding: 'utf8', timeout: 15000 });
    assert.ifError(result.error);
    return result;
  };
  return { root, env, config, command, cleanup };
}

async function tomlExample(reference, index = 0) {
  const text = (await readFile(path.join(bundle, 'references', reference), 'utf8')).replace(/\r\n/g, '\n');
  const blocks = [...text.matchAll(/```toml\n([\s\S]*?)\n```/g)];
  assert.ok(blocks[index], `${reference} TOML example ${index}`);
  return blocks[index][1];
}

test('installed copy is self-contained and uses portable skill metadata', async (t) => {
  const { root } = await fixture(t);
  const installed = path.join(root, '.agents/skills/nook');
  await cp(bundle, installed, { recursive: true, errorOnExist: true, force: false });
  const main = (await readFile(path.join(installed, 'SKILL.md'), 'utf8')).replace(/\r\n/g, '\n');
  assert.match(main, /^---\nname: nook\ndescription: .+\n/);
  assert.ok(main.split('\n').length <= 150, 'keep the common entrypoint short');
  const files = ['SKILL.md', ...(await readdir(path.join(installed, 'references'))).map(name => `references/${name}`)];
  for (const file of files) {
    const content = await readFile(path.join(installed, file), 'utf8');
    for (const [, link] of content.matchAll(/\]\(([^)]+)\)/g)) {
      if (/^https:\/\//.test(link)) continue;
      const target = path.resolve(path.dirname(path.join(installed, file)), link.split('#')[0]);
      assert.ok(target.startsWith(`${installed}${path.sep}`), `${file}: link escapes installed bundle: ${link}`);
      await readFile(target);
    }
  }
});

test('documented CLI forms parse and contradictory options fail', async (t) => {
  const { root, config, command } = await fixture(t);
  assert.equal(path.normalize(command(['config', 'path']).stdout.trim()), config);
  for (const args of [
    ['run', '--name', 'web'], ['run', '--config', 'configs/dev.toml', '--local'],
    ['run', '--runner'], ['run', '--only', 'api'], ['run', '--worktree=agent-a'],
    ['stop', 'web.localhost'], ['alias', 'remove', 'docs'], ['prune'],
    ['list'], ['status'], ['tailscale', 'status'], ['config', 'show'],
    ['config', 'path'], ['ca', 'export', 'caddy-local-ca.pem'],
  ]) {
    const result = command([...args, '--help']);
    assert.equal(result.status, 0, result.stderr);
  }
  const printed = command(['init', '--print', '--name', 'web', '--', 'node', 'server.js']);
  assert.equal(printed.status, 0, printed.stderr);
  assert.match(printed.stdout, /command = \["node", "server.js"\]/);
  assert.ok(!(await readdir(root)).includes('nook.toml'));
  for (const args of [
    ['run', '--runner', '--only', 'api'], ['run', '--local'], ['run', '--strict-port'],
  ]) {
    const result = command(args);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /cannot be used|required arguments/);
    assert.doesNotMatch(result.stderr, /Caddy Admin API request failed/);
  }
});

test('published TOML examples resolve before dependency access', async (t) => {
  const { root, command } = await fixture(t);
  await writeFile(path.join(root, 'nook.toml'), await tomlExample('configuration.md'));
  const resolved = args => {
    const result = command(args);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Caddy Admin API request failed/, result.stderr);
  };
  resolved(['run']);
  await writeFile(path.join(root, 'nook.local.toml'), await tomlExample('configuration.md', 1));
  resolved(['run']);
  await mkdir(path.join(root, 'configs'));
  await writeFile(path.join(root, 'configs/dev.toml'), await tomlExample('configuration.md'));
  resolved(['run', '--config', 'configs/dev.toml']);
  const missing = command(['run', '--config', 'configs/dev.toml', '--local']);
  assert.notEqual(missing.status, 0);
  assert.doesNotMatch(missing.stderr, /Caddy Admin API request failed/);
  await writeFile(path.join(root, 'configs/nook.local.toml'), await tomlExample('configuration.md', 1));
  resolved(['run', '--config', 'configs/dev.toml', '--local']);
  await rm(path.join(root, 'nook.local.toml'));
  for (const app of ['api', 'web']) await mkdir(path.join(root, 'apps', app), { recursive: true });
  await writeFile(path.join(root, 'nook.toml'), await tomlExample('workspaces.md'));
  // Naming/config validation does not need a live Git worktree in this check.
  resolved(['run', '--no-worktree']);
  resolved(['run', '--no-worktree', '--only', 'api']);
  // Selection-dependent interpolation runs after reconciliation; check it with
  // real Caddy below rather than pretending it precedes dependency access.
  resolved(['run', '--no-worktree', '--only', 'web']);
  resolved(['run', '--no-worktree', '--runner', '--', 'node', 'runner.mjs']);
});

async function until(check, description, diagnostics = () => '') {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    if (await check()) return;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.fail(`timed out: ${description}\n${diagnostics()}`);
}

async function http(port, hostname, pathname = '/', ca) {
  return new Promise(resolve => {
    const request = (ca ? getHttps : get)({ host: '127.0.0.1', port, path: pathname,
      headers: { Host: hostname }, servername: hostname, ca, timeout: 1000 }, response => {
      let body = '';
      response.on('data', chunk => { body += chunk; });
      response.on('end', () => resolve({ status: response.statusCode, body }));
    });
    request.on('timeout', () => request.destroy());
    request.on('error', error => resolve({ error: `${error.code}: ${error.message}` }));
  });
}

test('real Caddy preserves foreign routes across documented app and workspace launches',
  { skip: process.env.NOOK_AGENT_E2E !== '1', timeout: 120000 }, async (t) => {
    const { root, env, config, command, cleanup } = await fixture(t);
    const server = createServer();
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    const adminPort = server.address().port;
    await new Promise(resolve => server.close(resolve));
    const caddyConfig = path.join(root, 'Caddyfile');
    await writeFile(caddyConfig, `{
  admin 127.0.0.1:${adminPort}
  auto_https disable_redirects
  skip_install_trust
}
https://foreign.localhost {
  bind 127.0.0.1
  tls internal
  respond "foreign-sentinel"
}
`);
    await writeFile(config, `format_version = 1\ncaddy_admin = "http://127.0.0.1:${adminPort}"\n`);
    const children = [];
    const responses = new Map();
    const diagnostics = () => [
      `Last HTTPS responses: ${JSON.stringify(Object.fromEntries(responses))}`,
      ...children.map(child => `Process ${child.pid} (exit=${child.exitCode}, signal=${child.signalCode}):\n${child.log}`),
    ].join('\n');
    function start(executable, args, cwd = root) {
      const child = spawn(executable, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
      child.log = '';
      child.stdout.on('data', chunk => { child.log += chunk; });
      child.stderr.on('data', chunk => { child.log += chunk; });
      child.on('error', error => { child.log += String(error); });
      children.push(child);
      return child;
    }
    cleanup.push(async () => {
      for (const child of [...children].reverse()) {
        if (child.exitCode === null && child.signalCode === null) {
          const exited = once(child, 'close');
          child.kill();
          await Promise.race([exited, new Promise(resolve => setTimeout(resolve, 3000))]);
          if (child.exitCode === null && child.signalCode === null) {
            child.kill('SIGKILL');
            await exited;
          }
        }
      }
    });
    const caddy = start(process.platform === 'win32' ? 'caddy.exe' : 'caddy', ['run', '--config', caddyConfig]);
    await until(async () => {
      assert.equal(caddy.exitCode, null, caddy.log);
      return (await http(adminPort, `127.0.0.1:${adminPort}`, '/config/'))?.status === 200;
    }, 'isolated Caddy Admin API');
    const caFile = path.join(root, 'test-ca.pem');
    const exported = command(['ca', 'export', caFile]);
    assert.equal(exported.status, 0, exported.stderr);
    // Trust only this temporary Caddy CA for these requests, never the host store.
    const ca = await readFile(caFile);
    const app = `require('node:http').createServer((req,res)=>res.end(JSON.stringify({url:process.env.NOOK_URL,port:process.env.PORT,api:process.env.API_URL}))).listen(Number(process.env.PORT),process.env.HOST);`;
    await writeFile(path.join(root, 'server.js'), app);
    await writeFile(path.join(root, 'nook.toml'), await tomlExample('configuration.md'));
    const neighbor = start(binary, ['run', '--name', 'neighbor']);
    const responds = async (hostname) => {
      const response = await http(443, hostname, '/', ca);
      responses.set(hostname, response);
      return response?.status === 200 && response.body.includes(`https://${hostname}`);
    };
    await until(() => responds('neighbor.localhost'), 'neighbor ready', diagnostics);
    const standalone = start(binary, ['run']);
    await until(() => responds('web.localhost'), 'documented standalone ready', diagnostics);
    assert.match(standalone.log, /domain=web\.localhost url=https:\/\/web\.localhost port=\d+/);
    assert.equal(command(['stop', 'web.localhost']).status, 0);
    await until(() => standalone.exitCode !== null || standalone.signalCode !== null, 'standalone stopped');

    const git = spawnSync('git', ['init', '-q'], { cwd: root, env, encoding: 'utf8' });
    assert.equal(git.status, 0, git.stderr);
    for (const id of ['api', 'web']) {
      await mkdir(path.join(root, 'apps', id), { recursive: true });
      await writeFile(path.join(root, 'apps', id, 'server.js'), app);
    }
    const wrapper = path.join(repository, 'packages/nook-run/bin/nook-run.js');
    // A minimal external runner retains package-local cwd and forwards the session.
    await writeFile(path.join(root, 'runner.mjs'), `import {spawn} from 'node:child_process';
const children=['api','web'].map(id=>spawn(process.execPath,[${JSON.stringify(wrapper)},'--','node','server.js'],{cwd:'apps/'+id,stdio:'inherit',env:process.env}));
for(const child of children)child.on('exit',code=>process.exit(code??1));`);
    const pathKey = Object.keys(env).find(key => key.toUpperCase() === 'PATH');
    const inheritedPath = env[pathKey] ?? '';
    delete env[pathKey];
    env.PATH = `${path.dirname(binary)}${path.delimiter}${inheritedPath}`;
    await writeFile(path.join(root, 'nook.toml'), await tomlExample('workspaces.md'));
    const missingApp = command(['run', '--worktree=agent-a', '--only', 'web']);
    assert.notEqual(missingApp.status, 0);
    assert.match(missingApp.stderr, /api/);
    assert.doesNotMatch(command(['list']).stdout, /agent-a\.shop\.localhost/);
    for (const runner of [false, true]) {
      const args = ['run', '--worktree=agent-a'];
      if (runner) args.push('--runner', '--', 'node', 'runner.mjs');
      const child = start(binary, args);
      const api = 'api.agent-a.shop.localhost';
      const web = 'web.agent-a.shop.localhost';
      await until(async () => {
        assert.equal(child.exitCode, null, child.log);
        return await responds(api) && await responds(web);
      }, `workspace ready (runner=${runner})`, diagnostics);
      const apiBody = JSON.parse((await http(443, api, '/', ca)).body);
      const webBody = JSON.parse((await http(443, web, '/', ca)).body);
      assert.notEqual(apiBody.port, webBody.port);
      assert.equal(webBody.api, `http://127.0.0.1:${apiBody.port}`);
      assert.ok(child.log.includes(`url=https://${web}`), child.log);
      const stopped = command(['stop', api]);
      assert.equal(stopped.status, 0, stopped.stderr);
      await until(() => child.exitCode !== null || child.signalCode !== null, 'workspace exited');
      const pruned = command(['prune']);
      assert.equal(pruned.status, 0, pruned.stderr);
      const listed = command(['list']);
      assert.equal(listed.status, 0, listed.stderr);
      assert.doesNotMatch(listed.stdout, /agent-a\.shop\.localhost/);
      assert.ok(await responds('neighbor.localhost'));
      assert.equal((await http(443, 'foreign.localhost', '/', ca))?.body, 'foreign-sentinel');
    }
    assert.equal(command(['stop', 'neighbor.localhost']).status, 0);
    await until(() => neighbor.exitCode !== null || neighbor.signalCode !== null, 'neighbor stopped');
    assert.equal(command(['prune']).status, 0);
  });
