# @neotamia/nook-run

Run a JavaScript development command through a system-installed [Nook](https://github.com/NeoTamia/NTNook) CLI and get actionable installation guidance when Nook is missing.

## Install

```sh
pnpm add --save-dev @neotamia/nook-run
# or: npm install --save-dev @neotamia/nook-run
# or: yarn add --dev @neotamia/nook-run
# or: bun add --dev @neotamia/nook-run
```

Nook and Caddy remain system dependencies. This package does not download software and has no install lifecycle script.

## Use

```json
{
  "scripts": {
    "dev": "nook-run --name web -- vite"
  }
}
```

All arguments are passed directly to `nook run` without an intermediate shell:

```sh
nook-run --name api --app-port 3000 --strict-port -- node server.js
```

The command inherits the terminal and environment, forwards `SIGINT` and `SIGTERM`, and returns the launched process's exit code.

When Nook is not installed, `nook-run` prints a warning and runs the command after `--` directly.
This lets the project start on a new development machine without Nook's local domains or HTTPS
features. Native Nook installations on Linux and Windows are discovered through `PATH`.
