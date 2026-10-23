import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { installationGuidance } from "../bin/install-guidance.js";
import {
  shouldLaunchFallback,
  shouldSignalChild,
} from "../bin/signal-forwarding.js";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const launcher = path.join(packageRoot, "bin", "nook-run.js");

async function temporaryDirectory(t) {
  const directory = await mkdtemp(path.join(tmpdir(), "nook-run-test-"));
  t.after(() => rm(directory, { force: true, recursive: true }));
  return directory;
}

function runLauncher(args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(options.runtime ?? process.execPath, [launcher, ...args], {
      cwd: options.cwd,
      env: options.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";

    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({ code, signal, stderr, stdout }));
  });
}

async function createFakeNook(directory, source) {
  const executable = path.join(directory, "nook");
  await writeFile(executable, `#!${process.execPath}\n${source}`, "utf8");
  await chmod(executable, 0o755);
  return executable;
}

test("warns and runs the command directly when Nook is missing", async (t) => {
  const directory = await temporaryDirectory(t);
  const command = path.join(directory, "fake-app");
  await writeFile(
    command,
    `#!${process.execPath}\nconsole.log(JSON.stringify(process.argv.slice(2)));\n`,
    "utf8",
  );
  await chmod(command, 0o755);

  const result = await runLauncher(["--name", "web", "--", command, "an argument"], {
    env: { ...process.env, PATH: directory },
  });

  assert.equal(result.code, 0);
  assert.match(result.stderr, /warning: Nook was not found in PATH/);
  assert.match(result.stderr, /Nook was not found in PATH/);
  assert.match(result.stderr, /nook-installer\.sh/);
  assert.match(result.stderr, /local domains and HTTPS will be unavailable/);
  assert.equal(result.stdout, '["an argument"]\n');
});

test("never runs an unmanaged fallback when a workspace marker is present", async (t) => {
  const directory = await temporaryDirectory(t);
  for (const marker of ["", path.join(directory, "stale-session.json")]) {
    const result = await runLauncher(["--", process.execPath, "-e", 'console.log("unmanaged")'], {
      env: { ...process.env, PATH: directory, NOOK_WORKSPACE_SESSION: marker },
    });
    assert.equal(result.code, 127);
    assert.match(result.stderr, /cannot join the workspace session/);
    assert.doesNotMatch(result.stderr, /running the command directly/);
    assert.equal(result.stdout, "");
  }
});

test("selects installation guidance for the host platform", () => {
  const linuxGuidance = installationGuidance("linux");
  assert.match(linuxGuidance, /nook-installer\.sh/);
  assert.match(linuxGuidance, /\| sh/);

  const windowsGuidance = installationGuidance("win32");
  assert.match(windowsGuidance, /Windows from PowerShell/);
  assert.match(windowsGuidance, /nook-installer\.ps1/);
  assert.match(windowsGuidance, /\| iex/);
  assert.doesNotMatch(windowsGuidance, /nook-installer\.sh/);
});

test("lets Windows broadcast Ctrl+C to Nook for graceful cleanup", () => {
  assert.equal(shouldSignalChild("win32", "SIGINT"), false);
  assert.equal(shouldSignalChild("win32", "SIGTERM"), true);
  assert.equal(shouldSignalChild("linux", "SIGINT"), true);
});

test("does not start a fallback command after an interrupt", () => {
  assert.equal(shouldLaunchFallback(undefined), true);
  assert.equal(shouldLaunchFallback("SIGINT"), false);
  assert.equal(shouldLaunchFallback("SIGTERM"), false);
});

test("distinguishes a non-executable Nook binary", async (t) => {
  const directory = await temporaryDirectory(t);
  const executable = path.join(directory, "nook");
  await writeFile(executable, "not executable\n", { mode: 0o644 });

  const result = await runLauncher(["--", "fake-app"], {
    env: { ...process.env, PATH: directory },
  });

  assert.equal(result.code, 126);
  assert.match(result.stderr, /not executable/);
});

test("passes arguments, environment and working directory without a shell", async (t) => {
  const directory = await temporaryDirectory(t);
  const recordPath = path.join(directory, "record.json");
  await createFakeNook(
    directory,
    `
const { writeFileSync } = require("node:fs");
writeFileSync(process.env.NOOK_RUN_RECORD, JSON.stringify({
  argv: process.argv.slice(2),
  cwd: process.cwd(),
  marker: process.env.NOOK_RUN_MARKER,
  session: process.env.NOOK_WORKSPACE_SESSION,
}));
`,
  );

  const args = [
    "--lan",
    "--name",
    "my app",
    "--",
    "fake-app",
    "argument with spaces",
    "$(must-not-run)",
    "semi;colon",
  ];
  const result = await runLauncher(args, {
    cwd: directory,
    env: {
      ...process.env,
      NOOK_RUN_MARKER: "inherited",
      NOOK_WORKSPACE_SESSION: "opaque-session-context",
      NOOK_RUN_RECORD: recordPath,
      PATH: `${directory}${path.delimiter}${process.env.PATH ?? ""}`,
    },
  });
  const record = JSON.parse(await readFile(recordPath, "utf8"));

  assert.equal(result.code, 0);
  assert.deepEqual(record.argv, ["run", ...args]);
  assert.equal(record.cwd, directory);
  assert.equal(record.marker, "inherited");
  assert.equal(record.session, "opaque-session-context");
});

test("preserves Nook's exit code", async (t) => {
  const directory = await temporaryDirectory(t);
  await createFakeNook(directory, "process.exit(42);\n");

  const result = await runLauncher(["--", "fake-app"], {
    env: {
      ...process.env,
      PATH: `${directory}${path.delimiter}${process.env.PATH ?? ""}`,
    },
  });

  assert.equal(result.code, 42);
});

test("converts a terminating child signal to its conventional exit code", async (t) => {
  const directory = await temporaryDirectory(t);
  await createFakeNook(directory, 'process.kill(process.pid, "SIGKILL");\n');

  const result = await runLauncher(["--", "fake-app"], {
    env: {
      ...process.env,
      PATH: `${directory}${path.delimiter}${process.env.PATH ?? ""}`,
    },
  });

  assert.equal(result.code, 137);
});

test("forwards SIGTERM and reports the conventional signal exit code", async (t) => {
  const directory = await temporaryDirectory(t);
  const readyPath = path.join(directory, "ready");
  const signalPath = path.join(directory, "signal");
  await createFakeNook(
    directory,
    `
const { writeFileSync } = require("node:fs");
writeFileSync(process.env.NOOK_RUN_READY, "ready");
process.once("SIGTERM", () => {
  writeFileSync(process.env.NOOK_RUN_SIGNAL, "SIGTERM");
  process.exit(0);
});
setInterval(() => {}, 1000);
`,
  );

  const child = spawn(process.execPath, [launcher, "--", "fake-app"], {
    env: {
      ...process.env,
      NOOK_RUN_READY: readyPath,
      NOOK_RUN_SIGNAL: signalPath,
      PATH: `${directory}${path.delimiter}${process.env.PATH ?? ""}`,
    },
    stdio: "ignore",
  });

  const deadline = Date.now() + 5_000;
  while (true) {
    try {
      await readFile(readyPath);
      break;
    } catch (error) {
      if (error.code !== "ENOENT" || Date.now() >= deadline) {
        child.kill("SIGKILL");
        throw error;
      }
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  }

  child.kill("SIGTERM");
  const exit = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({ code, signal }));
  });

  assert.deepEqual(exit, { code: 143, signal: null });
  assert.equal(await readFile(signalPath, "utf8"), "SIGTERM");
});

const bunRuntime = process.env.NOOK_TEST_BUN;

test("Bun preserves the command boundary, Nook options and child separators", { skip: !bunRuntime }, async (t) => {
  const directory = await temporaryDirectory(t);
  await createFakeNook(directory, "console.log(JSON.stringify(process.argv.slice(2)));\n");
  for (const args of [
    ["--", "fake app", "argument with spaces", "--", "--name", "child"],
    ["--name", "web", "--", "fake-app", "--", "child"],
    ["--help"],
    [],
  ]) {
    const result = await runLauncher(args, {
      runtime: bunRuntime,
      env: { ...process.env, PATH: directory },
    });
    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(JSON.parse(result.stdout), ["run", ...args]);
  }
});

test("Bun standalone fallback preserves argv and refuses a stale session", { skip: !bunRuntime }, async (t) => {
  const directory = await temporaryDirectory(t);
  const args = ["--", process.execPath, "-e", "console.log(JSON.stringify(process.argv.slice(1)))", "--", "literal value", "--"];
  const environment = { ...process.env, PATH: directory };
  delete environment.NOOK_WORKSPACE_SESSION;
  const result = await runLauncher(args, { runtime: bunRuntime, env: environment });
  assert.equal(result.code, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), ["literal value", "--"]);
  assert.match(result.stderr, /running the command directly/);

  const session = await runLauncher(args, {
    runtime: bunRuntime,
    env: { ...environment, NOOK_WORKSPACE_SESSION: "stale-session" },
  });
  assert.equal(session.code, 127);
  assert.equal(session.stdout, "");
  assert.match(session.stderr, /refusing to start an unmanaged command/);
});
