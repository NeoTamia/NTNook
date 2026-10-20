# @neotamia/nook-run

Run a JavaScript development command through a system-installed [Nook](https://github.com/NeoTamia/NTNook) CLI and get actionable installation guidance when Nook is missing.

When started inside a Nook workspace runner session, the same wrapper joins its
app's prepared configuration. Package scripts need no second wrapper. See
[monorepo workspaces](https://github.com/NeoTamia/NTNook/blob/dev/docs/WORKSPACES.md) for Turbo's environment allowlist
and the root `nook run --runner -- <dev command>` invocation.

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
nook-run --name web --lan -- vite
```

With `--lan`, Nook prints an HTTP `.local` URL without a port, served through Caddy on port 80.
The application keeps its random port and loopback bind by default. Add
`--run-bind-address 0.0.0.0` to also allow direct `IP:port` access. Recognized framework CLIs receive host/port flags automatically; other applications
must listen on `HOST`/`PORT` or receive equivalent flags.
This requires a system Nook version that supports `--lan`; the direct fallback does not provide
mDNS announcements.

LAN mode can also be enabled with `lan = true` in `nook.toml` or `nook.local.toml`.
The local file overrides the shared file, including `lan = false`; `--lan` and `--no-lan`
override either setting.

The command inherits the terminal and environment, forwards `SIGINT` and `SIGTERM`, and returns the launched process's exit code.

When Nook is not installed, `nook-run` prints a warning and runs the command after `--` directly.
This lets the project start on a new development machine without Nook's local domains or HTTPS
features. Native Nook installations on Linux and Windows are discovered through `PATH`.
