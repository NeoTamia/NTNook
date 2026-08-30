# Nook

Nook is a Linux CLI that exposes local applications under stable `*.localhost` domains by configuring an existing Caddy instance.

## Requirements and installation

- Linux;
- Caddy `2.11.x`, running natively or in Docker, already started and accessible through its Admin API;
- unambiguous Caddy servers listening on `:443` for HTTPS and, when `--no-tls` is used, on `:80` for HTTP.

Recommended installation for the prebuilt Linux x86-64 binary:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/NeoTamia/NTNook/releases/latest/download/nook-installer.sh | sh
nook --help
```

The script installs Nook into `$XDG_BIN_HOME`, or `~/.local/bin` by default, without using `sudo`.
Use `NOOK_INSTALL_DIR` to select another directory and `NOOK_VERSION` to install a specific
version. Rust users can also build the published version from crates.io:

```sh
cargo install ntnook --locked
```

To build the repository locally:

```sh
caddy version
cargo install --path .
nook --help
```

Nook neither starts nor installs Caddy. It never invokes `sudo`, modifies `/etc/hosts`, or installs the local CA. Names under `.localhost` are resolved natively to loopback by compatible browsers and operating systems.

To run Caddy in Docker without installing its binary on the host, use the [Docker guide](docs/DOCKER.md). The official image is supported; `caddy-docker-proxy` is compatibility-tested, with a caveat concerning its reloads.

## Bash and Zsh completion

Nook generates completion scripts synchronized with the commands and options of the installed
version. To load them only in the current session:

```sh
# Bash
source <(nook completions bash)

# Zsh
autoload -Uz compinit
compinit
source <(nook completions zsh)
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

Regenerate the file after every Nook update. This initial version completes canonical forms such
as `nook run --name api` and `nook alias set api 3000`. The `nook api run` and
`nook alias api 3000` shortcuts, as well as existing run or alias names, are not yet completed
dynamically.

## Prepare Caddy for Nook

Nook adds its routes to an existing Caddy server; it does not create listeners itself. For HTTPS routes, the Caddyfile must produce exactly one server explicitly listening on `:443`. For example, add this site to your existing configuration:

```caddyfile
https://localhost {
	tls internal
	respond 404
}
```

If every command uses `--no-tls`, no HTTPS server is required. Caddy must then provide exactly one HTTP server on `:80`; Nook neither issues nor verifies certificates for these routes.

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
- `--app-port <port>` requests a port, with an explicit fallback if it is occupied;
- `--strict-port` disables that fallback and requires `--app-port`;
- `--force` transfers a hostname already owned by Nook without stopping the old process;
- `--config <path>` explicitly selects the project file;
- `--local` applies the `nook.local.toml` next to a file selected with `--config`;
- `--readiness-warn-after <seconds>` sets the readiness warning delay;
- arguments after `--` are passed through directly, without an implicit shell.

Nook replaces `{port}` literally in each argument and injects `PORT`, `HOST` (the value of `run_bind_address`, `127.0.0.1` by default), and `NOOK_URL`. The process receives the terminal's stdin/stdout/stderr, and its exit code is preserved even if Caddy cleanup must be retried later.

After reserving the route and starting the process, Nook always prints the selected domain, public URL, and effective application port, including when the name and port are inferred:

```text
nook: domain=api.localhost url=https://api.localhost port=5173
```

This information is written to stderr to keep supervision messages separate from the application's standard output.

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
`PATH`. The wrapper requires Node.js 22 or newer; Windows users must run it with Nook inside WSL.

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
- `stop` sends SIGTERM to the currently managed run's process group;
- `stop --force` waits up to two seconds, then uses SIGKILL if the same process is still alive;
- `prune` removes dead leases and orphaned routes, replays pending operations, and restores missing routes;
- `update` downloads the latest GitHub release, verifies its SHA-256 checksum, and replaces a binary installed by the installation script;
- `update --force` reinstalls the latest release even when the installed version is already current;
- `update --check` reports whether a newer version exists without installing it.

A binary installed with Cargo must be updated with `cargo install ntnook --locked --force`. Nook warns on stderr when an update is available; `NOOK_DISABLE_UPDATE_CHECK=1` disables this check.

Nook never changes a foreign Caddy route, even with `--force`. Nook routes carry an owner UUID, so an old process cannot delete its replacement's route.

## Project configuration

Create a documented project configuration in the current directory:

```sh
nook init
nook init --name api --app-port 5173 -- pnpm run dev
nook init --local
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
app_port = 5173
strict_port = false
readiness_warn_after_seconds = 30
```

Without a command after `--`, `command` is required. Name precedence is: `--name`, project file, Git root basename, then current-directory basename. CLI values override file values.

Each developer can add a `nook.local.toml` in the same directory. Its fields override those in
`nook.toml` without changing the shared configuration:

```toml
format_version = 1
name = "api-alwyn"
app_port = 5180
strict_port = true
```

The complete precedence order is: defaults and inference, `nook.toml`, `nook.local.toml`, then
CLI options. The local file can also be used on its own, without `nook.toml`. Each file is
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
error preserves its exit code and does not restart the server outside the proxy. This recipe uses
the POSIX shell because Nook is currently limited to Linux.

## Global configuration

The global file is `$XDG_CONFIG_HOME/nook/config.toml`, falling back to `~/.config/nook/config.toml`:

Nook can create it, show its effective configuration, and change a value:

```sh
nook config init
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

`run_bind_address` selects the interface used to reserve the port, probe readiness, and inject `HOST`. `caddy_loopback_host` changes only the connection address for local upstreams as seen by Caddy. `caddy_client_ip_ranges` controls the `remote_ip` matcher added to every Nook route. The defaults preserve native loopback behavior.

## Export the local CA

When Caddy is not installed on the host, export its public certificate through the Admin API:

```sh
nook ca export caddy-local-ca.pem
nook ca export caddy-local-ca.pem --force
```

Nook prints the SHA-256 fingerprint, refuses to overwrite by default, and never installs the certificate. With Caddy in Docker, the CA remains stable as long as the `/data` volume is preserved.

Versioned state resides in `$XDG_STATE_HOME/nook/state.json`, falling back to `~/.local/state/nook/state.json`. Writes are atomic and locked; do not edit this registry while Nook is running.

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

## MVP scope

The MVP manages one service per project, local Caddy routes, persistent aliases, Linux processes, and recovery on the next CLI invocation.

Out of scope: a permanent daemon, IPC or a local socket, an implicit shell, modifying `/etc/hosts`, installing or starting Caddy, automatic CA installation, Docker lifecycle orchestration, LAN/mDNS, multiple services or workspaces, native Windows/macOS support, Tailscale Serve/Funnel, and any public exposure.

## Development

```sh
cargo fmt --check
cargo check
cargo test -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```

Integration tests require Caddy `2.11.x`, OpenSSL, Python 3, curl with HTTP/2, `unshare`, and `ip`. They use only loopback ports and temporary directories, disable trust installation, and clean up their processes and files. The full Nook/Caddy test opens `:80` and `:443` in an isolated user network namespace; CI runs the same test on its disposable runner.

Linux CI enforces this gate with the toolchain pinned in `rust-toolchain.toml`. Each `v*` tag then
produces a static Linux x86-64 binary, its SHA-256 checksum, and a GitHub attestation, before
publishing the `ntnook` source package on crates.io. See the
[traceability matrix](docs/TRACEABILITY.md) and [release notes](RELEASE.md).
