export function shouldSignalChild(platform, signal) {
  // Windows broadcasts Ctrl+C to every process attached to the console. Calling
  // child.kill("SIGINT") there would terminate Nook instead of letting its
  // console handler perform graceful route and process cleanup.
  return platform !== "win32" || signal !== "SIGINT";
}
