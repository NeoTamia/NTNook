import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const launcher = path.join(packageRoot, "bin", "nook-run.js");

async function temporaryDirectory(t) {
  const directory = await mkdtemp(path.join(tmpdir(), "nook-run-test-"));
  t.after(() => rm(directory, { force: true, recursive: true }));
  return directory;
}

function runLauncher(args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [launcher, ...args], {
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

test("prints installation guidance when Nook is missing", async (t) => {
  const directory = await temporaryDirectory(t);
  const result = await runLauncher(["--", "fake-app"], {
    env: { ...process.env, PATH: directory },
  });

  assert.equal(result.code, 127);
  assert.match(result.stderr, /Nook was not found in PATH/);
  assert.match(result.stderr, /nook-installer\.sh/);
  assert.match(result.stderr, /On Windows, run Nook from WSL/);
  assert.equal(result.stdout, "");
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
}));
`,
  );

  const args = [
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
      NOOK_RUN_RECORD: recordPath,
      PATH: `${directory}${path.delimiter}${process.env.PATH ?? ""}`,
    },
  });
  const record = JSON.parse(await readFile(recordPath, "utf8"));

  assert.equal(result.code, 0);
  assert.deepEqual(record.argv, ["run", ...args]);
  assert.equal(record.cwd, directory);
  assert.equal(record.marker, "inherited");
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
