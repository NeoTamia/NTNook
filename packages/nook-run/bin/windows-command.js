import { randomUUID } from "node:crypto";
import { statSync } from "node:fs";
import path from "node:path";

function environmentValue(environment, name) {
  const entry = Object.entries(environment).find(
    ([key]) => key.toLowerCase() === name.toLowerCase(),
  );
  return entry?.[1];
}

function regularFile(candidate) {
  try {
    return statSync(candidate).isFile();
  } catch {
    return false;
  }
}

function executableExtensions(environment) {
  const value = environmentValue(environment, "PATHEXT") ?? ".COM;.EXE;.BAT;.CMD";
  return value
    .split(";")
    .filter(Boolean)
    .map((extension) =>
      extension.startsWith(".") ? extension : `.${extension}`,
    );
}

export function resolveWindowsCommand(
  executable,
  { cwd, environment, isFile = regularFile },
) {
  const containsSeparator = /[\\/]/u.test(executable);
  const directories = containsSeparator
    ? [undefined]
    : [
        cwd,
        ...(environmentValue(environment, "PATH") ?? "")
          .split(";")
          .filter(Boolean)
          .map((entry) => entry.replace(/^"|"$/gu, "")),
      ];
  const extensions = path.win32.extname(executable)
    ? [""]
    : ["", ...executableExtensions(environment)];

  for (const directory of directories) {
    const base =
      directory === undefined
        ? path.win32.resolve(cwd, executable)
        : path.win32.join(directory, executable);
    for (const extension of extensions) {
      const candidate = `${base}${extension}`;
      if (isFile(candidate)) {
        return candidate;
      }
    }
  }
  return undefined;
}

function batchCommandLine(executable, args, percentVariable) {
  const values = [executable, ...args];
  const rendered = values.map((value, index) => {
    if (/[\r\n]/u.test(value)) {
      throw new TypeError("batch command arguments cannot contain line breaks");
    }
    const escaped = value
      .replaceAll("%", `%${percentVariable}%`)
      .replaceAll('"', '""');
    const quoted = index === 0 || value.length === 0 || /[ \t"&|<>^()%]/u.test(value);
    return quoted ? `"${escaped}"` : escaped;
  });
  return `"${rendered.join(" ")}"`;
}

export function prepareWindowsFallback(
  executable,
  args,
  { cwd, environment, isFile },
) {
  const resolved = resolveWindowsCommand(executable, { cwd, environment, isFile });
  if (resolved === undefined || !/\.(?:cmd|bat)$/iu.test(resolved)) {
    return {
      executable: resolved ?? executable,
      args,
      options: {},
    };
  }

  const percentVariable = `NOOK_RUN_INTERNAL_PERCENT_${randomUUID().replaceAll("-", "")}`;
  const commandLine = batchCommandLine(resolved, args, percentVariable);
  return {
    executable: environmentValue(environment, "COMSPEC") ?? "cmd.exe",
    args: ["/D", "/V:OFF", "/S", "/C", commandLine],
    options: {
      env: { ...environment, [percentVariable]: "%" },
      windowsVerbatimArguments: true,
    },
  };
}
