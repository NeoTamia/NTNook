#!/usr/bin/env node

import { spawn } from "node:child_process";
import { writeSync } from "node:fs";
import { constants } from "node:os";

const INSTALL_URL =
  "https://github.com/NeoTamia/NTNook/releases/latest/download/nook-installer.sh";
let launchError = false;
let forwardedSignal;

function report(message) {
  writeSync(process.stderr.fd, `${message}\n`);
}

function signalExitCode(signal) {
  const signalNumber = constants.signals[signal];
  return signalNumber === undefined ? 1 : 128 + signalNumber;
}

const child = spawn("nook", ["run", ...process.argv.slice(2)], {
  cwd: process.cwd(),
  env: process.env,
  shell: false,
  stdio: "inherit",
});

function forwardSignal(signal) {
  forwardedSignal ??= signal;

  if (child.pid !== undefined) {
    child.kill(signal);
  }
}

function removeSignalHandlers() {
  process.removeListener("SIGINT", onSigint);
  process.removeListener("SIGTERM", onSigterm);
}

function onSigint() {
  forwardSignal("SIGINT");
}

function onSigterm() {
  forwardSignal("SIGTERM");
}

process.once("SIGINT", onSigint);
process.once("SIGTERM", onSigterm);

child.once("spawn", () => {
  if (forwardedSignal !== undefined) {
    child.kill(forwardedSignal);
  }
});

child.once("error", (error) => {
  launchError = true;
  removeSignalHandlers();

  if (error.code === "ENOENT") {
    report(`nook-run: Nook was not found in PATH.

Install Nook on Linux with:
  curl --proto '=https' --tlsv1.2 -LsSf ${INSTALL_URL} | sh

Then restart your terminal and run the development command again.
On Windows, run Nook from WSL.`);
    process.exitCode = 127;
    return;
  }

  if (error.code === "EACCES") {
    report("nook-run: the Nook executable in PATH is not executable.");
    process.exitCode = 126;
    return;
  }

  report(`nook-run: failed to start Nook: ${error.message}`);
  process.exitCode = 1;
});

child.once("close", (code, signal) => {
  removeSignalHandlers();

  if (launchError) {
    return;
  }

  if (signal !== null) {
    process.exitCode = signalExitCode(signal);
    return;
  }

  if (forwardedSignal !== undefined && code === 0) {
    process.exitCode = signalExitCode(forwardedSignal);
    return;
  }

  process.exitCode = code ?? 1;
});
