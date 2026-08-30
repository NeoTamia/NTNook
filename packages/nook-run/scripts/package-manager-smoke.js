import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageManager = process.env.PACKAGE_MANAGER;
const supportedPackageManagers = new Set(["npm", "pnpm", "yarn", "bun"]);

if (!supportedPackageManagers.has(packageManager)) {
  throw new Error(`Unsupported PACKAGE_MANAGER: ${packageManager ?? "undefined"}`);
}

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const fixture = await mkdtemp(path.join(tmpdir(), `nook-run-${packageManager}-`));
const binDirectory = path.join(fixture, "fake-bin");
const recordPath = path.join(fixture, "record.json");
const packageManagerEnvironment = {
  ...process.env,
  BUN_INSTALL_CACHE_DIR: path.join(fixture, ".bun-cache"),
  BUN_TMPDIR: path.join(fixture, ".bun-tmp"),
  npm_config_cache: path.join(fixture, ".npm-cache"),
  XDG_CACHE_HOME: path.join(fixture, ".cache"),
};

async function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? fixture,
      env: options.env ?? process.env,
      shell: false,
      stdio: options.capture ? ["ignore", "pipe", "pipe"] : "inherit",
    });
    let stderr = "";
    let stdout = "";

    if (options.capture) {
      child.stderr.setEncoding("utf8");
      child.stdout.setEncoding("utf8");
      child.stderr.on("data", (chunk) => {
        stderr += chunk;
      });
      child.stdout.on("data", (chunk) => {
        stdout += chunk;
      });
    }

    child.once("error", reject);
    child.once("close", (code, signal) => {
      if (code !== 0) {
        reject(
          new Error(
            `${command} ${args.join(" ")} failed with ${signal ?? code}\n${stderr}`,
          ),
        );
        return;
      }
      resolve({ stderr, stdout });
    });
  });
}

try {
  await mkdir(packageManagerEnvironment.BUN_TMPDIR, { recursive: true });
  await writeFile(
    path.join(fixture, "package.json"),
    JSON.stringify(
      {
        name: `nook-run-${packageManager}-smoke`,
        private: true,
        scripts: {
          dev: "nook-run --name smoke -- fake-app argument",
        },
      },
      null,
      2,
    ),
  );

  const installArguments = {
    npm: ["install", "--ignore-scripts", "--no-audit", "--no-fund", packageRoot],
    pnpm: [
      "add",
      "--save-dev",
      "--ignore-scripts",
      "--store-dir",
      path.join(fixture, ".pnpm-store"),
      packageRoot,
    ],
    yarn: ["add", "--dev", "--mode=skip-build", packageRoot],
    bun: ["add", "--dev", "--ignore-scripts", packageRoot],
  }[packageManager];
  await run(packageManager, installArguments, { env: packageManagerEnvironment });

  await mkdir(binDirectory, { recursive: true });
  await writeFile(
    path.join(binDirectory, "nook"),
    `#!${process.execPath}\n` +
      `require("node:fs").writeFileSync(process.env.NOOK_RUN_RECORD, JSON.stringify(process.argv.slice(2)));\n`,
  );
  await chmod(path.join(binDirectory, "nook"), 0o755);

  await run(packageManager, ["run", "dev"], {
    capture: true,
    env: {
      ...packageManagerEnvironment,
      NOOK_RUN_RECORD: recordPath,
      PATH: `${binDirectory}${path.delimiter}${process.env.PATH ?? ""}`,
    },
  });

  assert.deepEqual(JSON.parse(await readFile(recordPath, "utf8")), [
    "run",
    "--name",
    "smoke",
    "--",
    "fake-app",
    "argument",
  ]);
  console.log(`nook-run smoke test passed with ${packageManager}`);
} finally {
  await rm(fixture, { force: true, recursive: true });
}
