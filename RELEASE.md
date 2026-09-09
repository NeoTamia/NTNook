# Nook 0.1.0 — MVP release notes

GitHub Releases provide a static Linux archive and a native Windows archive:

- `nook-x86_64-unknown-linux-musl.tar.xz`, its SHA-256 checksum, and `nook-installer.sh`;
- `nook-x86_64-pc-windows-msvc.zip`, its SHA-256 checksum, and `nook-installer.ps1`.

Build-ready sources are also published on crates.io as `ntnook`.

## crates.io initialization for maintainers

Because the name `nook` already belongs to another project, the package is published as `ntnook`
while installing the `nook` command. Trusted Publishing requires an initial manual publication:

```sh
cargo publish --locked
```

Then add a GitHub Actions Trusted Publisher to the `ntnook` crate with repository
`NeoTamia/NTNook`, workflow `publish.yml`, and environment `release`. The GitHub `release`
environment must allow `v*` tags. After the first successful run, crates.io can be configured in
“Trusted Publishing only” mode.

## npm initialization for maintainers

The `@neotamia/nook-run` package has its own version, changelog, Release Please component, and
`nook-run-v*` tags. Its releases are independent from the Rust crate and do not publish a Nook
binary.

Before its first automated release, merge the package implementation into `dev` and wait for CI
to pass. From that exact commit with a clean worktree, publish the `packages/nook-run` package once
manually under a non-default bootstrap tag to reserve the name, then configure npm Trusted
Publishing:

```sh
git status --short
cd packages/nook-run
pnpm install --frozen-lockfile
pnpm test
pnpm publish --access public --tag bootstrap
```

Configure the npm package with repository `NeoTamia/NTNook`, workflow
`.github/workflows/publish.yml`, and GitHub environment `release`. New trusted-publisher
configurations must allow the npm publish action. The GitHub `release` environment must allow
`nook-run-v*` tags. Once OIDC publication succeeds, deprecate the `0.0.0` bootstrap version and
remove any temporary npm automation token; normal releases require no `NPM_TOKEN` and include npm
provenance automatically. The automated job uses pnpm for installation, tests, and packaging, but
must invoke `npm publish`: npm Trusted Publishing currently exchanges OIDC credentials only for
the npm CLI's `publish` and `stage publish` commands.

## Delivered features

- supervised launch of a local application under a `*.localhost` domain over HTTPS or HTTP;
- persistent aliases to a port or HTTP(S) URL;
- atomic, ownership-aware mutation of routes in an existing Caddy instance;
- `list`, `status`, `stop`, `prune`, and `update` commands, with recovery after interruption;
- static Bash, Zsh, and PowerShell completion generation with `nook completions`;
- TLS verification for HTTPS upstreams and diagnostics for Caddy's local CA.

## Platform and dependencies

- Linux x86-64 and Windows x86-64;
- Caddy `2.11.x`, native or through the official Docker image, started separately with an accessible Admin API;
- unambiguous Caddy servers on `:443` and, for `--no-tls`, on `:80`.

Native `caddy.exe` over its loopback TCP Admin API is the primary Windows path. Docker Desktop
with the official image is a supported secondary path.

The reproducible build uses Rust `1.98.0` and `Cargo.lock`. OpenSSL, Python 3, curl with HTTP/2,
util-linux, and iproute2 are required only by the Linux integration tests, not by the binary.

## Out of scope

The binary provides no daemon, IPC or embedded server, implicit shell, hosts-file modification,
Caddy installation or startup, automatic CA installation, Docker orchestration, LAN/mDNS,
multiple services, native macOS support, Tailscale Serve/Funnel, or public exposure.

The repository provides a supported official Caddy Compose setup on Linux and tested compatibility with caddy-docker-proxy. `nook ca export` retrieves the public CA without a Caddy executable on the host. See `docs/DOCKER.md`.

Before publication, artifacts must come from green Linux and Windows CI runs. Verify the Linux
archive with `sha256sum --check nook-x86_64-unknown-linux-musl.tar.xz.sha256`; the PowerShell
installer verifies the Windows archive automatically with `Get-FileHash`.
