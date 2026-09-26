# Coding standards

Review contract for the `ntnook` crate (binary `nook`). Short rules; the README remains the product description.

## Audience

- **Implementers** make the change work and leave the [CI gate](#hard-ci-gate) green.
- **Reviewers** apply this file. That includes humans and separate review agents (Arch and Review bots). CodeRabbit and Codex are out of this loop. A blocking finding cites a rule below. Anything else is a suggestion.

## Hard CI gate

`rust-toolchain.toml` pins channel `1.98.0` with `clippy` and `rustfmt`. No pull request merges unless this gate from the README Development section is green on Linux and on Windows when the change applies there:

```sh
cargo fmt --check
cargo check
cargo test -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```

`.github/workflows/ci-crate.yml` runs those checks with `--locked` on `ubuntu-24.04` and `windows-2025`, then builds the release binary. Integration tests need Caddy `2.11.x` (official `caddy.exe` on Windows). Linux also needs OpenSSL, Python 3, curl with HTTP/2, `unshare`, and `ip`.

## Architecture

`ntnook` is a binary crate. `src/main.rs` is `#![deny(unsafe_code)]` and the modules below are private. The CLI is the public interface. Keep process, Caddy, and OS details behind the module that already owns them:

| Module | Owns |
| --- | --- |
| `cli` | Parsing, terminal output, exit-code policy |
| `config` | Global and project TOML, hostname resolution |
| `caddy` | Admin API and canonical proxy targets |
| `process` | Port allocation, child argv and env, signals, readiness |
| `reconcile` | Partial operations, dead leases, missing routes |
| `state` | Versioned aliases, leases, and recovery operations |
| `platform` | Atomic file replace and directory sync |
| `update` | GitHub release check and binary self-replacement |

Prefer a small `pub(crate)` surface and a deep module. In review, flag a shallow module (callers still have to know Caddy JSON, Windows job objects, or Unix sockets) and a god file (a new concern dropped into `cli` or `process` when a neighbor already owns it). The Windows `MoveFileExW` path in `platform` is the existing `unsafe` exception. Do not add another.

## Tests

Reject:

- Tautological tests: asserting a constant, a string the test just formatted, or a field the test just wrote.
- Mocks that cannot fail: always `Ok`, never asserted, or a stub the production path cannot diverge from.
- Structure-sensitive unit tests that lock private call order or field layout when the observable result is the route, exit code, stderr line, or registry.

Prefer behavior and integration tests. Module tests sit next to the code (`caddy::tests`, `process::tests`, `state::tests`). Tests under `tests/` talk to a real Caddy Admin API.

README constraints:

- Always pass `--test-threads=1`. Tests share loopback ports, the Admin API, and process state.
- Loopback ports and temporary directories only. Install no CA. Clean up processes and files.
- Do not require `sudo`, a hosts-file edit, or a Unix socket on Windows.

## Platform and safety invariants

These match the README. Breaking one is a reject.

- Do not start or install Caddy, and do not orchestrate Docker. On Windows, native `caddy.exe` is primary; Docker Desktop is secondary (`docs/DOCKER.md`).
- Do not elevate. Do not call `sudo`. Installers stay in the user directory.
- Do not edit the hosts file. `.localhost` resolves to loopback without it.
- Do not run `caddy trust` or install the local CA. `status` may print the command; the user runs it.
- Unix Admin sockets and `--caddy-socket` are Linux-only. Windows uses an HTTP(S) Admin API URL.
- Every Nook route carries an owner UUID. A previous owner must not delete its replacement. `--force` may transfer a Nook-owned hostname and must leave foreign Caddy routes alone.
- Registry writes are locked and atomic (`state::Store::mutate`: temp file, then `platform::replace_file`). The registry does not store child argv.
- Preserve the child exit code, including when Caddy cleanup is only journaled for retry. Nook failures use `Error::exit_code` and `std::process::exit` in `main`.
- Supervision goes to stderr (`nook: domain=…`, `warning:`, `error:`). The child inherits the terminal. Keep Nook messages off the application's stdout.
- `stop` signals the managed process tree (SIGTERM on Unix, CTRL_BREAK on Windows). `stop --force` then terminates that same tree. Do not signal unrelated processes.

## Framework and argv injection

On `dev`, `process` replaces `{port}` in the child argv and injects `PORT`, `HOST`, and `NOOK_URL`. Arguments after `--` are not passed through a shell. `dev` has no framework detector.

`origin/feature/framework-support` is the reference for argv alignment (not merged here). Reviewers apply the rules below to that branch and to later injection. Detection reads the child argv. It does not read `package.json`, and `package.json` is not the contract.

- Add host/port flags only when the argv *is* Vite, Nuxt/`nuxi`, Next, Nitro, or Astro, including `npx`, `bunx`, `pnpx`, and exec forms (`npm exec`, `pnpm dlx`, `bun x`).
- `bun run`, `npm run`, and Elysia (`bun --watch`) get `PORT`, `HOST`, and `NOOK_URL` only.
- Forced mode (`--framework vite`, or `framework = "vite"`) still rewrites flags only on the matching framework executable. Leave unrelated `npx`, `bunx`, and package-manager arguments alone (`npx eslint .`, `bunx tsc`, `npx -p vite`, `bun run dev`).
- Honor opt-out: `--no-framework` and `framework = "none"` skip detection and flag injection. `--no-framework` conflicts with `--framework`.
- Non-serving invocations (`vite build`, `next build`, and the other non-server subcommands) do not gain host/port flags, including under a forced framework.
- An existing `--host` or `--port` on the framework CLI may be overwritten to Nook's bind address and reserved port. Wrapper flags stay as the user wrote them.

## Pull request review

Each blocking finding comes with a concrete patch or diff. Style comments that rustfmt and Clippy already enforce are not findings.

Every PR body includes a blast radius:

```markdown
## Blast radius

One-way: ...
Two-way: ...
```

- **One-way** — costly to undo once shipped: state migrations, installers, release and publish, CA trust or the certificate store, the route registry, Windows process-tree termination.
- **Two-way** — safe to revert: docs, UX copy, non-critical helpers.

Write `none` when a side does not apply. This file is two-way (docs only).

## Retro

When a human review comment states a durable rule, add a bullet under [Review log](#review-log) in this file, or in a checklist linked from that section. Do not repeat the same comment on the next PR. One-off line nits stay on the PR.

## Out of scope

README Scope, restated so a PR does not widen the product in passing:

A permanent daemon, IPC or a local socket, an implicit shell, hosts-file edits, installing or starting Caddy, automatic CA installation, Docker lifecycle orchestration, LAN/mDNS, multiple services or workspaces, native macOS, Tailscale Serve/Funnel, and any public exposure.

Also see `docs/DOCKER.md`, `docs/TRACEABILITY.md`, and `RELEASE.md`.

## Review log

Durable rules taken from human review. None yet.
