import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import {
  prepareWindowsFallback,
  resolveWindowsCommand,
} from "../bin/windows-command.js";

test("resolves Windows commands using PATHEXT order", () => {
  const existing = new Set([String.raw`C:\tools\vite.CMD`, String.raw`C:\tools\node.EXE`]);
  const options = {
    cwd: String.raw`C:\project`,
    environment: {
      Path: String.raw`C:\tools`,
      PathExt: ".CMD;.EXE",
    },
    isFile: (candidate) => existing.has(candidate),
  };

  assert.equal(resolveWindowsCommand("vite", options), String.raw`C:\tools\vite.CMD`);
  assert.equal(resolveWindowsCommand("node", options), String.raw`C:\tools\node.EXE`);
});

test("prepares Windows batch fallbacks without shell expansion", () => {
  const environment = {
    PATH: String.raw`C:\tools`,
    PATHEXT: ".CMD;.EXE",
    COMSPEC: String.raw`C:\Windows\System32\cmd.exe`,
    NOOK_TEST_VALUE: "expanded&whoami",
  };
  const invocation = prepareWindowsFallback(
    "vite",
    ["argument with spaces", "%NOOK_TEST_VALUE%", "value&whoami"],
    {
      cwd: String.raw`C:\project`,
      environment,
      isFile: (candidate) => candidate === String.raw`C:\tools\vite.CMD`,
    },
  );

  assert.equal(invocation.executable, environment.COMSPEC);
  assert.deepEqual(invocation.args.slice(0, 4), ["/D", "/V:OFF", "/S", "/C"]);
  assert.equal(invocation.options.windowsVerbatimArguments, true);
  assert.match(invocation.args[4], /"value&whoami"/u);
  const [internalName, internalValue] = Object.entries(invocation.options.env).find(
    ([key]) => key.startsWith("NOOK_RUN_INTERNAL_PERCENT_"),
  );
  assert.equal(internalValue, "%");
  assert.match(
    invocation.args[4],
    new RegExp(`%${internalName}%NOOK_TEST_VALUE%${internalName}%`, "u"),
  );
});

test("rejects line breaks in Windows batch fallback arguments", () => {
  assert.throws(
    () =>
      prepareWindowsFallback("vite.cmd", ["safe\nunexpected-command"], {
        cwd: String.raw`C:\project`,
        environment: {},
        isFile: () => true,
      }),
    /line breaks/u,
  );
});

test(
  "executes a Windows batch fallback with literal arguments",
  { skip: process.platform !== "win32" },
  async (t) => {
    const directory = await mkdtemp(path.join(tmpdir(), "nook-run-windows-"));
    t.after(() => rm(directory, { force: true, recursive: true }));
    await writeFile(
      path.join(directory, "fallback.cmd"),
      [
        "@echo off",
        'if "%~1"=="argument with spaces" if "%~2"=="%%NOOK_TEST_VALUE%%" if "%~3"=="value&whoami" exit /b 17',
        "exit /b 19",
        "",
      ].join("\r\n"),
      "utf8",
    );

    const environment = {
      ...process.env,
      PATH: directory,
      PATHEXT: ".CMD;.EXE",
      NOOK_TEST_VALUE: "expanded&whoami",
    };
    const invocation = prepareWindowsFallback(
      "fallback",
      ["argument with spaces", "%NOOK_TEST_VALUE%", "value&whoami"],
      { cwd: directory, environment },
    );
    const code = await new Promise((resolve, reject) => {
      const child = spawn(invocation.executable, invocation.args, {
        cwd: directory,
        stdio: "ignore",
        ...invocation.options,
      });
      child.once("error", reject);
      child.once("close", resolve);
    });

    assert.equal(code, 17);
  },
);
