# Configuration and one-app launches

## Reuse the project's command

For a project with an existing raw `dev:app` script:

```sh
nook run --name web -- npm run dev:app
```

The script must honor `HOST` and `PORT`, or pass equivalent arguments to its
server. Nook does not inspect arbitrary package scripts. Automatic Vite, Nuxt
and Astro alignment applies to recognized CLI argv; it does not add another
framework integration or rewrite framework configuration.

For a server that already honors those variables, a complete `nook.toml` is:

```toml
format_version = 1
name = "web"
command = ["node", "server.js"]
```

Run `nook run` from that directory. To preview a generated file without writing:

```sh
nook init --print --name web -- node server.js
```

Arguments after `--`, and `command` arrays, are argv without an implicit shell.
Nook replaces `{port}` in each argument and supplies `PORT`, `HOST` (loopback by
default), and `NOOK_URL` to the child. They do not become variables in the parent
terminal. Keep raw server scripts separate from scripts that invoke Nook to
avoid recursive launches.

## Overrides and ports

For one app, precedence is defaults/applicable global configuration,
`nook.toml`, `nook.local.toml`, then CLI. Each file declares its format version;
unknown fields are rejected. A local-only v1 configuration is also supported.

An optional `nook.local.toml` for a deliberately fixed port:

```toml
format_version = 1
app_port = 5173
strict_port = true
```

Prefer allocated ports unless the project needs a fixed one. `--app-port 5173`
allows fallback when occupied; `--strict-port` disables fallback and requires a
requested port. Use the reported effective port, not the preferred value.

```sh
nook run --config configs/dev.toml
nook run --config configs/dev.toml --local
```

The first command loads only the explicit file. The second requires and overlays
the neighboring `nook.local.toml`; `--local` cannot be used without `--config`.
When configuring a project, keep workstation-specific local files out of Git.
Nook does not update `.gitignore` for you.

## JavaScript wrapper

Install `@neotamia/nook-run` with the project's package manager when a portable
package script is wanted (for example `npm install --save-dev @neotamia/nook-run`).
Its supported Node baseline is 22. A package script can be:

```json
{"scripts":{"dev":"nook-run -- node server.js"}}
```

The wrapper invokes the system Nook through PATH and preserves its result. If
Nook is absent outside a workspace session, it warns and runs the command after
`--` directly, without Nook domains or HTTPS. With a workspace session marker,
missing Nook or an invalid session fails instead of starting an unmanaged app.
A Nook/Caddy/application failure is not a reason to retry outside the proxy.

## Prepared machine and platform differences

Inspect settings with `nook config show` and locate the global file with
`nook config path`. Ordinary launches do not need `config init` or a CA export.

| Setting | Linux | Windows |
| --- | --- | --- |
| Global config | `$XDG_CONFIG_HOME/nook/config.toml`, default `~/.config/nook/config.toml` | `%APPDATA%\Nook\config.toml` |
| State | `$XDG_STATE_HOME/nook/state.json`, default `~/.local/state/nook/state.json` | `%LOCALAPPDATA%\Nook\state.json` |
| Caddy Admin API | HTTP(S) or Unix socket | HTTP(S) URL; no Unix socket |
| Shell recipes | POSIX shell | PowerShell; use `$env:NAME` for environment |

On Windows, nonempty `XDG_CONFIG_HOME` and `XDG_STATE_HOME` override the
Windows locations above, using the same `nook` subdirectory as Linux.

Use native Caddy on Windows as the primary mode; Docker is secondary. Both
platforms support x86-64. There is no macOS support or custom-TLD/`.test` setup.
Do not edit the state file. Nook manages its locks, identities and pending work.

If machine preparation is actually missing, use the version-matched
[installation and Caddy guide](https://github.com/NeoTamia/NTNook/blob/a2a4ce5e6e65145dfc880c626e2903a1211d38d3/README.md#prepare-caddy-for-nook)
or [Docker guide](https://github.com/NeoTamia/NTNook/blob/a2a4ce5e6e65145dfc880c626e2903a1211d38d3/docs/DOCKER.md).
Report the missing prerequisite instead of installing/restarting Caddy or
trusting a CA as part of an ordinary app launch.
