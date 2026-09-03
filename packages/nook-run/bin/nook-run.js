#!/usr/bin/env node

import { spawn } from "node:child_process";
import { writeSync } from "node:fs";
import { constants } from "node:os";

import { installationGuidance } from "./install-guidance.js";
import { shouldSignalChild } from "./signal-forwarding.js";

let forwardedSignal;
let child;
const launchErrors = new WeakSet();

function report(message) {
  writeSync(process.stderr.fd, `${message}\n`);
}

function signalExitCode(signal) {
  const signalNumber = constants.signals[signal];
  return signalNumber === undefined ? 1 : 128 + signalNumber;
}

const args = process.argv.slice(2);
const separatorIndex = args.indexOf("--");
const command = separatorIndex === -1 ? [] : args.slice(separatorIndex + 1);

function spawnChild(executable, executableArgs) {
  child = spawn(executable, executableArgs, {
    cwd: process.cwd(),
    env: process.env,
    shell: false,
    stdio: "inherit",
  });

  child.once("spawn", () => {
    if (
      forwardedSignal !== undefined &&
      shouldSignalChild(process.platform, forwardedSignal)
    ) {
      child.kill(forwardedSignal);
    }
  });

  const spawnedChild = child;
  child.once("close", (code, signal) => handleClose(spawnedChild, code, signal));
  return child;
}

function forwardSignal(signal) {
  forwardedSignal ??= signal;

  if (
    child.pid !== undefined &&
    shouldSignalChild(process.platform, signal)
  ) {
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

function handleLaunchError(error) {
  launchErrors.add(child);

  if (error.code === "ENOENT") {
    report(`nook-run: warning: Nook was not found in PATH; running the command directly.

${installationGuidance(process.platform)}

Nook features such as local domains and HTTPS will be unavailable.`);

    if (command.length === 0) {
      removeSignalHandlers();
      report("nook-run: no command was provided after --.");
      process.exitCode = 127;
      return;
    }

    const fallback = spawnChild(command[0], command.slice(1));
    fallback.once("error", (fallbackError) => {
      launchErrors.add(fallback);
      removeSignalHandlers();
      report(`nook-run: failed to start ${command[0]}: ${fallbackError.message}`);
      process.exitCode = fallbackError.code === "ENOENT" ? 127 : 1;
    });
    return;
  }

  removeSignalHandlers();

  if (error.code === "EACCES") {
    report("nook-run: the Nook executable in PATH is not executable.");
    process.exitCode = 126;
    return;
  }

  report(`nook-run: failed to start Nook: ${error.message}`);
  process.exitCode = 1;
}

function handleClose(closedChild, code, signal) {
  if (launchErrors.has(closedChild)) {
    return;
  }

  removeSignalHandlers();

  if (signal !== null) {
    process.exitCode = signalExitCode(signal);
    return;
  }

  if (forwardedSignal !== undefined && code === 0) {
    process.exitCode = signalExitCode(forwardedSignal);
    return;
  }

  process.exitCode = code ?? 1;
}

spawnChild("nook", ["run", ...args]).once("error", handleLaunchError);
