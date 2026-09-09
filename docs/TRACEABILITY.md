# MVP traceability

This matrix links product requirements to implementation tickets and checks tracked in the repository. Pending post-MVP tickets are not part of the release gate.

| Verifiable requirement | Tickets | Test or verification |
| --- | --- | --- |
| A Linux/Windows Rust binary crate with internal modules and consistent public errors | NOOK-10, NOOK-32, NOOK-53 | Linux check plus `cargo check --target x86_64-pc-windows-msvc`; `src/main.rs` and platform guards |
| `run` CLI, short form, opaque arguments, and stable help | NOOK-11, NOOK-32 | `cli::tests::*` tests; `cli_alias::help_is_successful_*` |
| Versioned global/project configuration, management through `nook config`, and CLI → project → default precedence | NOOK-12 | `config::tests::*`, `cli::tests::parses_global_configuration_commands`, and `cli_config` tests |
| Name normalized as a DNS label with project/Git/directory fallback | NOOK-17 | `config::tests::name_priority_*`, `normalizes_valid_names`, `rejects_invalid_dns_labels` |
| Versioned XDG/Windows registry without argv, written atomically and locked | NOOK-15, NOOK-16, NOOK-38, NOOK-53 | `state::tests::*`, including Windows paths, concurrency and temporary-file recovery |
| Admin API client without starting Caddy and with actionable errors | NOOK-18, NOOK-32 | `caddy::tests::admin_client_*`; `cli_alias::status_has_a_stable_failure_*` |
| Safe discovery of `:443`/`:80` servers, overrides, and ambiguities | NOOK-18 | `discovers_*`, `available_server_*`, `ambiguity_*`, `no_tls_*` tests |
| Nook containers placed before catch-all without altering foreign routes | NOOK-19, NOOK-20 | `containers_partition_*`, `container_is_repositioned_*`, `empty_container_*` tests |
| Atomic replacement through `PATCH`, ETag with bounded retries, and reread | NOOK-21, NOOK-41 | `managed_backend_*`, `retries_re_read_*`, `fourth_precondition_*` tests; real Admin API in `nook_caddy_e2e` |
| UUID ownership, conditional cleanup, and protection from a previous owner | NOOK-23, NOOK-26, NOOK-38 | `owner_marker_*`, `stale_owner_cleanup_*`, CLI concurrency tests |
| Combined hostname + loopback-source matcher | NOOK-20, NOOK-41 | `proxy_route_combines_host_and_loopback_*`; non-loopback `proxy_protocols` test |
| Upstream port or HTTP(S) URL, strict validation, and TLS never disabled | NOOK-28, NOOK-41 | Caddy validation tests; valid/expired/untrusted/wrong-hostname `alias_tls` cases |
| Persistent aliases, short forms, idempotent removal, and `--force` limited to Nook | NOOK-26, NOOK-28, NOOK-29, NOOK-30, NOOK-38 | reconciliation tests; `cli_alias::alias_shortcuts_*`, `force_refuses_a_foreign_*` |
| Port allocation, `{port}`, environment, and no relaunch after a race | NOOK-22, NOOK-38 | process tests for `reserve_port`, `substitution`, `child_environment`, `lost_port_race_*` |
| Grouped process trees, readiness, interruption, forced stop, identity, and preserved exit code | NOOK-24, NOOK-25, NOOK-27, NOOK-32, NOOK-38, NOOK-57 | POSIX group tests and Windows Job Object/creation-time tests in `process` |
| No orphaned child/lease/route after an impossible spawn or supervisor death | NOOK-25, NOOK-27, NOOK-38 | `failed_spawn_*`, `caddy_failure_before_run_*`, `prune_recovers_after_*` |
| Convergent transaction journals at every mutation boundary | NOOK-13, NOOK-16, NOOK-38 | `recovers_journals_left_at_every_external_mutation_boundary`; reconciliation tests |
| Every operational command reconciles first; selection and timestamps persist | NOOK-13, NOOK-33, NOOK-38 | `ordinary_list_reconciles_reload_and_records_synchronization`; real Caddy reload in `nook_caddy_e2e` |
| Safe `list`, `status`, `stop`, `stop --force`, and `prune` | NOOK-13, NOOK-25, NOOK-33, NOOK-36, NOOK-38 | CLI/process/reconciliation tests, real forced stop, and reload/restoration harness |
| Drift diagnostics and CA trust without running privileged commands | NOOK-36 | `status_drift_*`, `local_ca_diagnostic_*`; untrusted-CA integration |
| Routes actually produced by Nook: HTTPS and HTTP-only run/alias, reload, concurrency, and foreign-route protection | NOOK-38, NOOK-41 | `tests/nook_caddy_e2e.rs` against the real Caddy Admin API and ports `:80`/`:443` |
| Configurable host bind, loopback-upstream translation, and Docker client ranges | NOOK-45 | `docker_network_settings_*`, `docker_route_translates_*`, process tests, and `tests/docker_e2e.sh` |
| Public CA export and fingerprint without a Caddy binary | NOOK-46 | `ca export` CLI tests, E2E validation with `curl --cacert` |
| Secure official Compose setup and persistent volumes | NOOK-47 | `docker/compose.yaml`, `docker/Caddyfile`, `docker compose config`, restart/fingerprint E2E |
| caddy-docker-proxy coexistence and restoration after reload | NOOK-48 | `docker/compose.caddy-docker-proxy.yaml`, label/reconciliation scenario |
| Official/proxy Docker CI gate | NOOK-49 | `docker` job in `.github/workflows/ci.yml` |
| Native `caddy.exe`, TCP Admin API, Windows trust store, and HTTP alias E2E | NOOK-50, NOOK-55 | `tests/windows_caddy_e2e.rs`; Windows `caddy::tests`; `windows` CI job |
| Docker Desktop as a secondary Windows mode | NOOK-50, NOOK-56 | `docker/nook-config.windows.toml.example`, `docs/DOCKER.md`, existing Docker gate |
| Windows ZIP, checksum, PowerShell installer/completion, and deferred self-update | NOOK-50, NOOK-54 | Windows build/publish jobs, installer parser validation, update tests |
| Docker documentation and cross-platform matrix | NOOK-51, NOOK-50, NOOK-56 | `docs/DOCKER.md`, README, release notes, and YouTrack specification |
| Preserved Host, forwarded headers, WebSocket, SSE, streaming, HTTP/2, 502, and upstream TLS | NOOK-41 | `tests/proxy_protocols.rs`; `tests/alias_tls.rs` |
| Bash/Zsh completion plus native PowerShell completion | NOOK-52, NOOK-54 | `tests/cli_completions.rs`; Windows CI generates and parses the PowerShell script |
| `nook update` replaces a GitHub binary after SHA-256 verification; a cached check warns when a newer version exists | post-MVP | `update::tests::*`, `cli_update` tests |
| Package-manager-neutral JavaScript wrapper reports a missing Nook and preserves arguments, exit codes, and signals | post-MVP | `packages/nook-run/test`; Node 22/24 and npm/pnpm/Yarn/Bun CI jobs |
| Documentation for requirements, safeguards, troubleshooting, and out-of-scope items | NOOK-31, NOOK-43 | `README.md`, `RELEASE.md` |
| Linux and Windows gates compile, format, lint, run native Caddy integrations, and produce verifiable binaries | NOOK-34, NOOK-35, NOOK-38, NOOK-41, NOOK-43, NOOK-58 | `.github/workflows/ci-crate.yml`; both archives, SHA-256 files, installers and attestations through `.github/workflows/publish.yml` |

## Release gate

Local and CI validation run, in this order:

```sh
cargo fmt --check
cargo check --locked
cargo test --locked -- --test-threads=1
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked --release
```

Integration tests are isolated in temporary directories, use Caddy `2.11.x`, install no CA, and clean up their processes. The MVP release requires every linked MVP ticket to be resolved; Tailscale work remains explicitly post-MVP and pending.
