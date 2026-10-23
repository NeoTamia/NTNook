# Nook

Nook is a Linux and Windows CLI that exposes local applications under stable `*.localhost`
domains by configuring an existing Caddy instance.

## Requirements and installation

- Linux x86-64 or Windows x86-64;
- Caddy `2.11.x`, running natively or in Docker, already started and accessible through its Admin API;
- unambiguous Caddy servers listening on `:443` for HTTPS and, when `--no-tls` or `--lan` is used, on `:80` for HTTP.

Recommended installation for the prebuilt Linux x86-64 binary:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/NeoTamia/NTNook/releases/latest/download/nook-installer.sh | sh
nook --help
```

The script installs Nook into `$XDG_BIN_HOME`, or `~/.local/bin` by default, without using `sudo`.
Use `NOOK_INSTALL_DIR` to select another directory and `NOOK_VERSION` to install a specific
version. It also installs Bash and Zsh completion files under `${XDG_DATA_HOME:-~/.local/share}`
and adds idempotent source blocks to `~/.bashrc` and `~/.zshrc`.

On Windows x86-64, run the PowerShell installer without elevation:

```powershell
irm https://github.com/NeoTamia/NTNook/releases/latest/download/nook-installer.ps1 | iex
nook --help
```

It verifies the release checksum, installs `nook.exe` under
`%LOCALAPPDATA%\Programs\Nook\bin`, adds that directory to the user `PATH`, and configures
PowerShell completion. `NOOK_INSTALL_DIR` and `NOOK_VERSION` provide the same overrides as the
Linux installer. Open a new terminal after the first installation so it receives the updated
`PATH`. If an active Nook process delays replacement during a reinstall, the installer reports the
operation as scheduled; a terminal replacement failure is persisted and shown by the next Nook or
installer invocation.

Rust users on either platform can also build the published version from crates.io:

```sh
cargo install ntnook --locked
```

To build the repository locally:

```sh
caddy version
cargo install --path .
nook --help
```

Nook neither starts nor installs Caddy. It never elevates privileges, modifies the hosts file, or
installs the local CA. Names under `.localhost` are resolved natively to loopback by compatible
browsers and operating systems.

To run Caddy in Docker without installing its binary on the host, use the [Docker guide](docs/DOCKER.md). The official image is supported; `caddy-docker-proxy` is compatibility-tested, with a caveat concerning its reloads.

## Shell completion

Nook generates completion scripts synchronized with the commands and options of the installed
version. The release installer installs both scripts and adds idempotent source blocks to
`~/.bashrc` and `~/.zshrc`; the manual setup below is useful for Cargo/source installations.
To load them only in the current session:

```sh
# Bash
source <(nook completions bash)

# Zsh
autoload -Uz compinit
compinit
source <(nook completions zsh)

# PowerShell
nook completions power-shell | Out-String | Invoke-Expression
```

For a persistent Bash installation:

```sh
completion_dir="${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions"
mkdir -p "$completion_dir"
nook completions bash > "$completion_dir/nook"
```

For Zsh, generate `_nook` in a function directory:

```sh
completion_dir="${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions"
mkdir -p "$completion_dir"
nook completions zsh > "$completion_dir/_nook"
```

Then add that directory to `fpath` in `.zshrc`, before the call to `compinit`:

```zsh
fpath=("${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions" $fpath)
autoload -Uz compinit
compinit
```

For a persistent manual PowerShell installation, generate the script and source it from your
profile:

```powershell
$completion = Join-Path $env:LOCALAPPDATA "Nook\completions\nook.ps1"
New-Item -ItemType Directory -Force (Split-Path $completion) | Out-Null
nook completions power-shell | Set-Content -Encoding utf8 $completion
Add-Content $PROFILE.CurrentUserAllHosts ". '$completion'"
```

Regenerate the file after every Nook update. The generated scripts complete canonical forms such
as `nook run --name api` and `nook alias set api 3000`, as well as existing run names after
`nook stop` and alias names after `nook alias remove`. They also offer the known names for the
`nook <name> run` and `nook alias <name>` shortcuts. Dynamic completion reads the local registry
without reconciling Caddy or changing state; an unavailable, invalid, or busy registry simply
produces no dynamic candidates.

## Prepare Caddy for Nook

Nook adds its routes to an existing Caddy server; it does not create listeners itself. For HTTPS routes, the Caddyfile must produce exactly one server explicitly listening on `:443`. For example, add this site to your existing configuration:

```caddyfile
https://localhost {
	tls internal
	respond 404
}
```

If every command uses `--no-tls`, no HTTPS server is required. Caddy must then provide exactly one HTTP server on `:80`; Nook neither issues nor verifies certificates for these routes.

### Windows: native Caddy (recommended)

Download the official `caddy.exe`, or install the community-maintained Chocolatey/Scoop package,
and place it on `%PATH%`. Caddy may run in a terminal or as a Windows service through `sc.exe` or
WinSW. Nook does not manage that lifecycle. Keep the Admin API on its loopback TCP default:

```caddyfile
{
	admin 127.0.0.1:2019
}

https://localhost {
	tls internal
	respond 404
}
```

Then start Caddy using your chosen service configuration and run:

```powershell
caddy version
nook status
```

If `local_ca` is reported as `not trusted`, run the exact `caddy trust --address ...` remediation
printed by Nook from a terminal with the required Windows permissions. Nook diagnoses the current
user's Windows `ROOT` certificate store but never modifies it. Unix socket addresses and
`--caddy-socket` are not supported on Windows; use an HTTP(S) Admin API URL.

### Linux: optional Unix Admin socket

If the Admin API should use the standard Unix socket, also place this directive in the existing global block of the Caddyfile:

```caddyfile
{
	admin "unix//run/caddy/admin.socket|0660"
}
```

Validate and reload Caddy:

```sh
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl reload caddy
```

The user running Nook must be able to traverse `/run/caddy` and read from and write to the socket. On an installation that uses the `caddy` group, add your user to that group once so that you can subsequently use Nook without `sudo`, then log out and start a new session:

```sh
sudo usermod -aG caddy "$USER"
```

After logging back in, verify that the new session has the group:

```sh
id -nG
```

The output must contain `caddy`. If `getent group caddy` lists the user but `id -nG` does not
contain `caddy`, the current session has not loaded the new membership yet: log out completely
and log back in.

Then check the socket permissions:

```sh
stat -c '%A %U:%G %n' /run/caddy/admin.socket
```

The `caddy` group must have write permission, for example
`srw-rw---- caddy:caddy`. The `|0660` suffix on the `admin` directive instructs Caddy to apply
this mode whenever it creates the socket. Do not replace it with a simple systemd
`ExecStartPost=chmod`: an Admin API configuration change can recreate the socket with mode
`0200` after the startup hook, immediately cutting off Nook's access.

Apply the configuration by restarting Caddy:

```sh
sudo systemctl daemon-reload
sudo systemctl restart caddy
nook status
```

Access through the `caddy` group allows the entire configuration to be changed through the Admin
API; grant it only to trusted users. Do not run Nook with `sudo`: its configuration and state
files belong to your user, and application processes should retain that user's normal permissions.

Caddy issues `*.localhost` certificates with its local CA. Explicitly install that CA from your user session so the system and browsers trust it:

```sh
caddy trust --address unix//run/caddy/admin.socket
nook --caddy-socket /run/caddy/admin.socket status
```

The second command should report `trusted` for `local_ca`. Fully close and restart any browsers that were already open. For a TCP Admin API, use the address shown by `nook status` instead, for example `caddy trust --address 127.0.0.1:2019`.

## Run an application

```sh
nook run --name api -- bun run dev
nook api run -- bun run dev
nook run --name docs --app-port 5173 --strict-port -- npm run dev
nook run --name legacy --no-tls -- ./server
```

`run` options:

- `--name <name>` selects the domain; `.localhost` is appended automatically;
- `--no-tls` uses the HTTP frontend exclusively;
- `--lan` exposes `http://<name>.local` through Caddy and announces the name via mDNS;
- `--no-lan` disables LAN mode enabled in a project configuration;
- `--run-bind-address <IP>` overrides the project and global bind address (IPv4 or IPv6), also injected as `HOST`;
- `--app-port <port>` requests a port, with an explicit fallback if it is occupied;
- `--strict-port` disables that fallback and requires `--app-port`;
- `--force` transfers a hostname already owned by Nook without stopping the old process;
- `--config <path>` explicitly selects the project file;
- `--local` applies the `nook.local.toml` next to a file selected with `--config`;
- `--readiness-warn-after <seconds>` sets the readiness warning delay;
- `--framework <name>` forces Vite, Nuxt, or Astro env alignment (`none` disables it);
- `--no-framework` disables detection and flag injection;
- arguments after `--` are passed through directly, without an implicit shell.

Nook replaces `{port}` literally in each argument and injects `PORT`, `HOST` (the value of `run_bind_address`, `127.0.0.1` by default), and `NOOK_URL`. When the child argv is Vite, Nuxt, or Astro (including `bunx` / `npx`), Nook also appends that CLI's flags and extra variables listed under [Framework alignment](#framework-alignment). `bun run` and Elysia (`bun --watch`) keep that base environment. The process receives the terminal's stdin/stdout/stderr, and its exit code is preserved even if Caddy cleanup must be retried later.

After reserving the route and starting the process, Nook always prints the selected domain, public URL, and effective application port, including when the name and port are inferred:

```text
nook: domain=api.localhost url=https://api.localhost port=5173
```

This information is written to stderr to keep supervision messages separate from the application's standard output.

### Access from other devices

Use `--lan` to open an application from another device on the same network:

```sh
nook run --name api --lan -- node server.js
nook run --name web --lan -- vite
```

The application keeps its dynamically allocated port and its configured bind address
(`127.0.0.1` by default). Caddy forwards `http://web.local` on port 80 to that application;
the `.local` URL has no port to enter. The local `https://web.localhost` URL stays available.

```text
nook: domain=web.localhost url=https://web.localhost port=5173
nook: lan_url=http://web.local
```

Caddy must have an explicit HTTP server on port 80, listening on all interfaces or a LAN IPv4
address. If needed, add this site to your existing Caddyfile and reload Caddy:

```caddyfile
http://:80 {
    respond 404
}
```

Nook adds and removes the `.local` route automatically. It keeps the existing client restrictions
on `.localhost` routes and does not change Caddy's listeners, Admin API, or firewall rules.
The LAN route accepts IPv4 clients on the HTTP listener; firewall rules determine which networks
can reach it. HTTPS remains available through the local `.localhost` route, while LAN access
uses HTTP and requires no certificate installation on other devices.

To **also allow direct access by IP and application port**, set the application bind address:

```sh
nook run --name web --lan --run-bind-address 0.0.0.0 -- vite
```

Nook then also prints URLs such as `http://192.168.1.20:5173`. With the default loopback bind,
other devices access the application through Caddy only. `--app-port 5173 --strict-port` can
fix the direct application port if needed.

Both choices can be saved in `nook.toml` or `nook.local.toml`:

```toml
format_version = 1
lan = true
# Optional: also expose the application port directly on all IPv4 interfaces.
# run_bind_address = "0.0.0.0"
```

LAN mode defaults to disabled. A local `lan = false` overrides a shared `lan = true`;
`--lan` and `--no-lan` override both files. With `--config`, the neighboring local file is
only applied when `--local` is passed. The bind address remains independent of `lan`, with
CLI > project/local configuration > global configuration precedence. `--no-lan` removes
`.local` exposure; it does not change an explicitly configured application bind address.

`HOST` and `PORT` contain the effective application bind address and port. `NOOK_LAN_URL`
contains the `.local` proxy URL. Recognized framework CLIs receive host/port flags automatically;
other applications must honor these variables or receive equivalent arguments. Nook also adds
the `.local` hostname to `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS` for Vite-based servers.

mDNS runs inside Nook on Linux and Windows. Devices need mDNS support, multicast discovery
(UDP 5353), and access to Caddy's HTTP port. The announcement and proxy route are withdrawn
when the run ends. If mDNS reports a name conflict, restart with another `--name` so that
Caddy and mDNS use the same hostname. If discovery is blocked, direct IP access requires
an application bind address reachable from the LAN.

LAN access can be combined with Tailscale Serve (`--tailscale`) or Funnel (`--funnel`).
`--no-tailscale` only disables Serve and Funnel.

### JavaScript package scripts

JavaScript projects can use `@neotamia/nook-run` to provide a package-manager-independent `dev`
script with an actionable error when the system `nook` executable is missing:

```sh
pnpm add --save-dev @neotamia/nook-run
# npm install --save-dev @neotamia/nook-run
# yarn add --dev @neotamia/nook-run
# bun add --dev @neotamia/nook-run
```

```json
{
  "scripts": {
    "dev": "nook-run --name web -- vite"
  }
}
```

The wrapper passes its arguments directly to `nook run`, inherits the terminal and environment,
forwards interruption signals, and preserves Nook's exit code. It has no dependencies or install
hooks and never downloads Nook or Caddy. Nook must still be installed separately and available in
`PATH`. The wrapper requires Node.js 22 or newer and invokes `nook` or `nook.exe` through `PATH`.

## Monorepo workspaces

A version 2 `nook.toml` can declare several `[[apps]]`, each with its own directory,
command and domain. `nook run` reserves their ports and routes before starting the
apps, and resolves references such as `${api.upstream}` in a frontend's environment.
Use `nook run --only api` to select one app, or `nook init --workspace` for a template.

For existing package scripts using `nook-run`, run the root development command
through `nook run --runner -- pnpm dev`. Each wrapper joins the prepared workspace
session and receives its app's port and URLs; the runner continues to launch the
package tasks. See [workspace configuration and runner setup](docs/WORKSPACES.md).

## Persistent aliases

```sh
nook alias set api 3000
nook alias api https://service.internal:8443 --preserve-host
nook alias set old 8080 --no-tls --force
nook alias list
nook alias remove api
nook alias --remove old
```

A target can be an integer port or an absolute `http://`/`https://` URL. Credentials, queries, fragments, and paths other than `/` are rejected. Upstream HTTPS certificates remain verified; no insecure mode is provided. An unavailable upstream does not remove the alias, and Caddy then returns `502`.

By default, the upstream receives its own `Host`. `--preserve-host` retains the requested domain. `X-Forwarded-Host` always retains the `.localhost` hostname.

## Operational commands

```sh
nook list
nook status
nook stop api
nook stop api --force
nook prune
nook update
nook update --check
nook update --force
```

- `list` distinguishes `starting`/`ready` runs from persistent aliases;
- `status` checks the Admin API, servers, Nook containers, drift, and local CA trust;
- `stop` gracefully signals the currently managed run's process group (SIGTERM on Unix,
  CTRL_BREAK on Windows);
- `stop --force` waits up to two seconds, then forcibly terminates the same process tree if it
  is still alive;
- `prune` removes dead leases and orphaned routes, replays pending operations, and restores missing routes;
- `update` downloads the latest GitHub release, verifies its SHA-256 checksum, and replaces a binary installed by the installation script;
- `update --force` reinstalls the latest release even when the installed version is already current;
- `update --check` reports whether a newer version exists without installing it.

A binary installed with Cargo must be updated with `cargo install ntnook --locked --force`. Nook warns on stderr when an update is available; `NOOK_DISABLE_UPDATE_CHECK=1` disables this check.
On Windows, Nook schedules the verified replacement after the current process exits. If that
deferred replacement fails, the next Nook invocation reports the persisted failure on stderr.

Nook never changes a foreign Caddy route, even with `--force`. Nook routes carry an owner UUID, so an old process cannot delete its replacement's route.

## Project configuration

Create a documented project configuration in the current directory:

```sh
nook init
nook init --name api --app-port 5173 -- pnpm run dev
nook init --local
nook init --local --lan
nook init --local --no-lan
nook init --local --run-bind-address 0.0.0.0
nook init --print
```

By default, `init` writes `nook.toml`; `--local` selects `nook.local.toml`, and `--print`
writes the generated TOML to standard output without changing the filesystem. Existing files are
protected unless `--force` is passed. The generated template documents every available project
setting, while command-line values are written as active settings.

The `nook.toml` file describes a single application:

```toml
format_version = 1
name = "api.neotamia"
command = ["bun", "run", "dev"]
tls = true
lan = false
app_port = 5173
strict_port = false
readiness_warn_after_seconds = 30
run_bind_address = "127.0.0.1"
# framework = "vite"
```

Without a command after `--`, `command` is required. Name precedence is: `--name`, project file, Git root basename, then current-directory basename. CLI values override file values.

### Framework alignment

`nook run` is enough. `@neotamia/nook-run` is only a convenience wrapper for package scripts.

```sh
nook run -- nuxt dev
nook run -- vite
nook run -- astro dev
nook run -- bunx vite
nook run -- bun --watch src/server.ts
```

Or in `nook.toml`: `command = ["nuxt", "dev"]` then `nook run`. Detection reads the child argv, including `bunx`, `npx`, `pnpx`, `npm exec`, `pnpm dlx`, and `bun x`. It leaves `package.json` unread.

Every child receives `PORT`, `HOST`, and `NOOK_URL`. `<bind>` below is the run bind address (`127.0.0.1` by default). `<port>` is the reserved application port. `<hostname>` is the Nook domain, such as `app.localhost`. An existing `--host` or `--port` on that framework CLI is overwritten with `<bind>` and `<port>`. `--strict-port` adds Vite's `--strictPort`.

| Framework | Argv Nook aligns | Flags Nook appends | Extra environment |
| --- | --- | --- | --- |
| Vite | `vite` (a version suffix such as `vite@latest` counts) | `--host <bind>`, `--port <port>`, and `--strictPort` when `--strict-port` is set | `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS=<hostname>` when that variable is unset or empty |
| Nuxt | `nuxt` or `nuxi` with `dev`, `start`, `preview`, or `serve` | `--host <bind>`, `--port <port>` | `NUXT_HOST=<bind>`, `NUXT_PORT=<port>`, and `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS=<hostname>` when that variable is unset or empty |
| Astro | `astro` with `dev`, `start`, `preview`, or `serve` | `--host <bind>`, `--port <port>`, `--allowed-hosts <hostname>` | `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS=<hostname>` when that variable is unset or empty |
| Bun, Elysia | `bun run`, `bun --watch src/server.ts`, and any other argv that is not a CLI above | none | none beyond `PORT`, `HOST`, and `NOOK_URL` |

Vite already accepts `localhost` and names under `.localhost`. Nook still sets `__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS` so the Nook hostname is explicit. Nuxt and Astro share that Vite check. Astro also receives `--allowed-hosts`.

Elysia has no host flag. A `bun --watch` server keeps the argv you wrote and sees `HOST` and `PORT`; pass those into `listen` in application code.

`--framework <vite|nuxt|astro|none>` forces that choice. `--no-framework` disables detection and injection. The two flags cannot be combined. `framework = "none"` in `nook.toml` is the same opt-out. A forced name still adds that framework's environment variables, and appends flags only when the argv contains that framework's executable. Automatic detection leaves `vite build`, `nuxt build`, and `astro build` on the base `PORT`, `HOST`, and `NOOK_URL` environment, with their argv unchanged.

Optional wrapper for `bun run` / `npm run`:

```json
{
  "scripts": {
    "dev": "nook-run -- nuxt dev"
  }
}
```

`bun run` without that wrapper, `bun --watch src/server.ts` (Elysia), and builds (`vite build`) only get `PORT` / `HOST` / `NOOK_URL`. Nuxt also gets `NUXT_HOST` / `NUXT_PORT` when the CLI is `nuxt`.

For a Bun/Elysia server, read `PORT` and `HOST` in the application's listener:

```ts
import { Elysia } from "elysia";

new Elysia()
  .get("/", () => "Hello")
  .listen({
    port: Number(process.env.PORT ?? 3000),
    hostname: process.env.HOST ?? "127.0.0.1",
  });
```

With plain `Bun.serve`, use the same `port` and `hostname` options. No framework flag or wrapper is needed for either server.

Each developer can add a `nook.local.toml` in the same directory. Its fields override those in
`nook.toml` without changing the shared configuration:

```toml
format_version = 1
name = "api-alwyn"
lan = true
app_port = 5180
strict_port = true
```

The complete precedence order is: defaults and global configuration, `nook.toml`,
`nook.local.toml`, then CLI options. The local file can also be used on its own, without
`nook.toml`. Each file is
validated separately, must declare `format_version = 1`, and rejects unknown fields.

Because this file is workstation-specific, add it to the project's `.gitignore`:

```gitignore
/nook.local.toml
```

Nook does not modify `.gitignore` or check whether Git tracks the file.

`--config path/custom.toml` remains deterministic and loads only the requested file. If a
`nook.local.toml` exists alongside it, Nook reports that it is ignored. Add `--local` explicitly
to overlay it:

```sh
nook run --config path/custom.toml --local
```

In this mode, `--local` fails if the neighboring file is absent and cannot be used without
`--config`.

### Fallback when Nook is not installed

To keep a `dev` script usable by a developer who has not yet installed Nook, separate the raw
application command and test for the binary directly in `dev`. Example with pnpm:

```json
{
  "scripts": {
    "dev": "if command -v nook >/dev/null 2>&1; then exec nook run; else printf '%s\\n' 'warning: Nook is not installed; starting without the local domain proxy' >&2; exec pnpm run dev:app; fi",
    "dev:app": "vite"
  }
}
```

The shared `nook.toml` then references the raw command:

```toml
format_version = 1
name = "app"
command = ["pnpm", "run", "dev:app"]
```

For npm, Yarn, or Bun, replace both occurrences of `pnpm` with `npm`, `yarn`, or `bun`,
respectively. The fallback runs only when the binary is absent: a Nook, Caddy, or application
error preserves its exit code and does not restart the server outside the proxy. This particular
recipe uses a POSIX shell; Windows projects should express the same fallback in a portable Node
script or invoke `nook-run` directly.

## Global configuration

On Linux, the global file is `$XDG_CONFIG_HOME/nook/config.toml`, falling back to
`~/.config/nook/config.toml`. On Windows it is `%APPDATA%\Nook\config.toml`.

Nook can create it, show its effective configuration, and change a value:

```sh
nook config init
# Linux only:
nook config init --caddy-socket /run/caddy/admin.socket
nook config show
nook config path
nook config set caddy-admin unix:///run/caddy/admin.socket
```

`config init` refuses to overwrite an existing file without `--force`. `config set` accepts the
`caddy-admin`, `https-server`, `http-server`, `run-bind-address`, `caddy-loopback-host`, and
`caddy-client-ip-ranges` keys. Use `auto` as a server value to remove its override, and separate
multiple IP ranges with commas.
`config show` displays the effective configuration, adding default values for missing fields.
`config path` prints only the raw file path, allowing commands such as
`bat "$(nook config path)"`.

```toml
format_version = 1
caddy_admin = "http://127.0.0.1:2019"
run_bind_address = "127.0.0.1"
caddy_loopback_host = "127.0.0.1"
caddy_client_ip_ranges = ["127.0.0.0/8", "::1"]

# Set only when discovery is ambiguous.
# https_server = "https"
# http_server = "http"
```

If Caddy's Admin API listens on a Unix socket, use its Caddy address directly:

```toml
caddy_admin = "unix//run/caddy/admin.socket"
```

The user running Nook must be able to traverse the directory and read from and write to the socket. The `unix:///run/caddy/admin.socket` URI form is also accepted.

For a one-off override without changing this file, pass the socket path directly:

```sh
nook --caddy-socket /run/caddy/admin.socket status
nook --caddy-socket /run/caddy/admin.socket run --name api --app-port 3000 -- command
```

For operational commands, the option can appear before or after the subcommand and temporarily
overrides `caddy_admin`. To save the socket in the configuration, use
`nook config init --caddy-socket PATH` or
`nook config set caddy-admin unix:///path/admin.socket`.

`run_bind_address` selects the interface used to reserve the port, probe readiness, and inject
`HOST`. It can be overridden per project in `nook.toml` or `nook.local.toml`.
`caddy_loopback_host` changes only the connection address for local upstreams as seen by Caddy.
`caddy_client_ip_ranges` controls the `remote_ip` matcher added to every Nook route. The defaults
preserve native loopback behavior.

## Tailscale

Nook can additionally publish a run or an alias to the devices of your tailnet with private
[Tailscale Serve](https://tailscale.com/kb/1312/serve). The local `.localhost` route stays
unchanged; Serve is an opt-in second entry point. Use `--funnel` instead to publish publicly
on the internet with [Tailscale Funnel](https://tailscale.com/docs/features/tailscale-funnel).

```sh
nook run --tailscale --name api -- bun run dev
nook alias set docs 5173 --tailscale
nook run --funnel --name demo -- bun run dev
nook alias set webhook 3000 --funnel
nook tailscale status
```

A project can expose every `nook run` by default from `nook.toml` or `nook.local.toml`:

```toml
tailscale = true
```

`--tailscale` enables the exposure for one run even when the project sets `tailscale = false`, and
`--no-tailscale` keeps one run local even when it sets `tailscale = true` or `funnel = true`; the two flags cannot be
combined. The usual precedence applies: `nook.toml`, then `nook.local.toml`, then the command line.
Without any of them, runs stay local. Aliases are not read from project files and only use
`--tailscale` or `--funnel`.

To expose every run publicly instead, set this in `nook.toml` or `nook.local.toml`:

```toml
funnel = true
```

`funnel = true` takes precedence if `tailscale = true` is also set. Set `funnel = false` in
`nook.local.toml` to disable the project's public default (Serve remains enabled if `tailscale`
is true). CLI flags override both settings: `--funnel` selects public Funnel, `--tailscale`
selects private Serve, and `--no-tailscale` keeps the run local. These flags cannot be combined.
Funnel must already be authorized for this device by the tailnet policy;
Nook checks the device capabilities before making changes and never starts the interactive
activation flow. Funnel URLs are accessible from the internet without tailnet membership.
Run and alias output labels them `exposure=public`, and `nook list` appends a `public` column.

Requirements: Tailscale 1.52 or newer installed and connected, HTTPS certificates enabled for the
tailnet, and a user allowed to change Serve (on Linux, root or the Tailscale operator). Nook runs
`tailscale` from `PATH` (`tailscale.exe` on Windows, falling back to
`%ProgramFiles%\Tailscale\tailscale.exe`); set `NOOK_TAILSCALE` to use another executable.
Serve reaches an exposed run through `127.0.0.1`, so its `run_bind_address` must be `127.0.0.1` or
`0.0.0.0`. Other addresses, including `::1` and `::`, are refused before anything changes.

All prerequisites are checked before Nook changes anything or spawns the child. A missing client,
an unreachable `tailscaled`, a disconnected device (with the login URL Tailscale reports), a
version without background Serve, or disabled HTTPS certificates (with the admin page where they
are enabled) is reported as an error and the command exits with status `1`.

Each private exposed run or alias takes the root of one tailnet HTTPS port: the first gets `443`
(`https://<device>.<tailnet>.ts.net`), then `8443`, `8444`, and so on. Ports used by any Serve
configuration Nook does not own are skipped, a name gets its previous port back when it is still
free, and allocation is recorded in the registry under the operations lock, so concurrent Nook
commands never receive the same port. The child additionally receives `NOOK_TAILSCALE_URL`, and the
run information line gains the tailnet URL:

```text
nook: domain=api.localhost url=https://api.localhost port=5173 tailscale_url=https://laptop.example.ts.net
```

Funnel uses the same lifecycle and `NOOK_TAILSCALE_URL`, but only allocates ports `443`, `8443`,
and `10000`, as required by the [Funnel CLI](https://tailscale.com/docs/reference/tailscale-cli/funnel).
Serve and Funnel share the same port pool: occupied ports are skipped, including foreign Funnel
permissions without a handler. If all three public ports are unavailable, Nook reports an error.
A previous Serve port outside that set is not reused for Funnel.

Nook removes an inherited `NOOK_TAILSCALE_URL` from every child it starts without exposure, for
example a local run started from an exposed one.

For detected Vite, Nuxt, and Astro dev servers, Nook also allows the exact tailnet hostname;
the local `.localhost` URL keeps working. Explicit allowed-host settings and an inherited
`__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS` remain authoritative and must include that hostname.

Nook also skips a port when a local server already accepts connections on it through one of the
device's tailnet addresses, that is a server listening on the wildcard address (`*:443`,
`0.0.0.0:443`, `[::]:443`) or directly on the tailnet IP. From this machine, such a server would
receive `https://<device>.<tailnet>.ts.net` instead of Tailscale Serve and fail the TLS handshake.
A Caddy that listens on `*:443` for the `.localhost` routes is the usual case. Servers listening only
on loopback do not count. Nook probes each candidate with a short TCP connection to the tailnet
addresses that `tailscale status` reports, then takes the next port and explains why on stderr:

```text
warning: skipped Tailscale Serve port 443: a local server already accepts connections on 100.101.102.103:443 (for example a web server listening on *:443), so https://laptop.example.ts.net would reach it instead of Tailscale Serve from this machine
```

`alias set --tailscale` prints a second `<tailnet URL> -> <target>` line, and `nook list` appends
the local and tailnet URLs to exposed entries:

```text
alias	persistent	docs.localhost	http://127.0.0.1:5173/	https://docs.localhost	https://laptop.example.ts.net:8443
```

Lifecycle:

- the Serve registration is created after the local Caddy route and before the child starts;
- a run removes it when it ends; an alias keeps it until `nook alias remove`, or until it is
  replaced without `--tailscale` or `--funnel`;
- `alias set --tailscale` publishes the alias under the same lock that records it. If Serve
  refuses, the previous alias, its route, and its tailnet URL stay in place. A forced replacement
  of an exposed alias therefore gets the next free port while the previous one is still served, and
  the previous port is released once the replacement is recorded;
- after a crash, the next Nook command (or `nook prune`) removes registrations whose run is gone.
  `nook prune` reports them in `tailscale_restored=<n> tailscale_removed=<n>` at the end of its
  summary line, next to `removed_dead` for the run itself:

  ```text
  restored=0 removed_dead=1 removed_orphans=0 completed_operations=0 tailscale_restored=0 tailscale_removed=1
  ```

- while Tailscale is stopped or unreachable, registrations are kept and reported as pending;
  after `tailscale up` (or a manual Serve reset) the next command restores them on the same port;
- registrations stay bound to the device's recorded MagicDNS name. If it changes after an account
  switch or device rename, Nook defers restoration and cleanup, refuses new exposure while old
  registrations remain, and `nook tailscale status` fails. Returning to the original identity
  resumes reconciliation. Remove existing exposures before switching or renaming if you intend
  to publish services under the new identity.

Safeguards:

- Serve proxies straight to the loopback application (`127.0.0.1` or `localhost`), so the
  `Tailscale-User-*` identity headers set by Tailscale reach it unchanged. Aliases with any other
  upstream are refused with either exposure flag. Funnel visitors are public and do not receive
  tailnet user identity headers; applications must provide their own authentication if needed;
- a private Serve registration is never silently made public. Nook refuses to keep it on
  a port that Funnel exposes unless it was explicitly created with `--funnel` or `funnel = true`. If Funnel is enabled later in the background,
  the next reconciliation withdraws Nook's root handler and forgets its registration, without
  restoring it automatically. Foreground sessions belong to their external caller and cannot be
  withdrawn with `serve off`: Nook warns, keeps tracking the registration, and `tailscale status`
  fails with a `public` state until the external session is stopped or Funnel is disabled.
  If the alias is removed or its run ends, Nook removes its own background handler so it cannot
  resume serving after that session ends, while leaving the external session untouched;
- Nook only runs `tailscale version`, `status --json`, `serve status --json`,
  `{serve,funnel} --bg --https=<port> <target>`, and `serve --https=<port> --set-path=/ off`.
  The scoped `serve off` removes the handler and its Funnel permission when no other paths remain,
  and works even if Funnel authorization has since been revoked. It never runs
  `serve reset`, `up`, `login`, `set`, or `cert`, never changes ACLs or tailnet settings, and never
  elevates. The tailnet policy remains the only authorization source;
- only the root handler is removed, and only when it still proxies to the upstream Nook recorded;
  other paths on the same port are preserved. A
  registration replaced by someone else is reported and left untouched. If an external change
  makes a Funnel registration private, Nook reports it as `foreign` instead of making it public again.

`nook tailscale status` checks the client, `tailscaled`, the login state, HTTPS, and the device
name, then lists each Nook registration as `active` (Serve), `active-public` (requested Funnel),
`missing`, `foreign`, or `pending`. An unexpected public registration that cannot be withdrawn is reported as `public` and makes the command fail. It
reconciles Tailscale registrations first but does not need Caddy, and exits with status `1` when
Serve cannot be used.

Registry format version 2 stores these registrations. Nook migrates a version 1 registry on its
next write; older Nook versions then report `unsupported state format_version 2`.

## Export the local CA

When Caddy is not installed on the host, export its public certificate through the Admin API:

```sh
nook ca export caddy-local-ca.pem
nook ca export caddy-local-ca.pem --force
```

Nook prints the SHA-256 fingerprint, refuses to overwrite by default, and never installs the certificate. With Caddy in Docker, the CA remains stable as long as the `/data` volume is preserved.

Versioned state resides in `$XDG_STATE_HOME/nook/state.json`, falling back to
`~/.local/state/nook/state.json`, on Linux and `%LOCALAPPDATA%\Nook\state.json` on Windows. Writes
are atomic and locked; do not edit this registry while Nook is running.

## Troubleshooting

- `Caddy Admin API request failed`: verify that Caddy is running and `caddy_admin` is correct. For a Unix socket, also check that the session belongs to the `caddy` group and that the Caddy directive uses `unix//run/caddy/admin.socket|0660`, as described in “Prepare Caddy for Nook.” An `ExecStartPost=chmod` alone does not survive socket recreation during configuration changes.
- `expected exactly one ... server; detected: none`: add the corresponding `:443` or `:80` listener in Caddy.
- multiple compatible servers detected: use the reported candidates to set `https_server` or `http_server`.
- `no selected Caddy HTTP server`: configure a `:80` listener before using `--no-tls`.
- `hostname ... foreign Caddy route`: choose another name or modify that route directly outside Nook.
- `drift detected` or pending cleanup: run `nook prune`.
- `local_ca not trusted`: manually run the displayed `caddy trust --address ...` command. Nook never runs it.
- readiness warning: verify that the application listens on `HOST` and `PORT`; the route and process remain active.
- strict port occupied: free the port, choose another one, or remove `--strict-port`.

## Scope

Nook manages standalone services and declared multi-app workspaces, local Caddy routes,
persistent aliases, supervised Linux or Windows process trees, and recovery on the next CLI invocation. Caddy may run natively on either
platform. On Windows, native `caddy.exe` is the primary supported mode; Docker Desktop remains a
secondary supported mode described in the [Docker guide](docs/DOCKER.md).

Out of scope: a permanent daemon, IPC or a local socket, an implicit shell, modifying the hosts
file, installing or starting Caddy, automatic CA installation, Docker lifecycle orchestration,
automatic worktree isolation, native macOS support, rewriting framework config files,
Angular-specific origin allow-lists, and public exposure outside Tailscale Funnel.
Private Tailscale Serve and public Tailscale Funnel are opt-in, as described in [Tailscale](#tailscale).

## Development

```sh
cargo fmt --check
cargo check
cargo test -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```

Linux integration tests require Caddy `2.11.x`, OpenSSL, Python 3, curl with HTTP/2, `unshare`, and
`ip`. Windows integration tests use the official `caddy.exe`. They use only loopback ports and
temporary directories, install no CA, and clean up their processes and files.

Linux and Windows CI enforce this gate with the toolchain pinned in `rust-toolchain.toml`. Each
`v*` tag produces Linux and Windows x86-64 archives, SHA-256 checksums, installers, and GitHub
attestations before publishing the `ntnook` source package on crates.io. See the
[traceability matrix](docs/TRACEABILITY.md) and [release notes](RELEASE.md).
