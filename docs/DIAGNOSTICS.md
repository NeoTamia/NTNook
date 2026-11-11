# Inspecting Nook without repairing it

Configure Caddy and its CA once, then use `nook run` as usual. When something is
wrong, `nook doctor` observes the current situation without reconciling it.

| Command | Behavior |
| --- | --- |
| `nook doctor` | Read-only inspection of configuration, recorded resources and Caddy |
| `nook doctor --deep` | The same inspection plus explicitly requested network probes |
| `nook status` | Reconcile first, then report the remaining state and CA trust |
| `nook prune` | Explicitly retry recovery, restore routes and clean dead/orphaned resources |

`list`, `alias list`, and `tailscale status` also reconcile before reporting.
The `list`, `alias list`, and `status` commands also support `--read-only` (and `--json` with that flag) through the [MCP observation API](MCP.md). Their default forms are not substitutes for read-only inspection. Reconciliation can change
routes, the registry, Tailscale registrations and workspace process/session
cleanup. A `status` result describes what remains **after** that intervention.

## Examples

```sh
nook doctor
nook doctor --json
nook doctor --config ./nook.toml --local
nook doctor --worktree=feature-a --only web
nook doctor --name api --no-tls --app-port 3000 -- python server.py
nook doctor --owner 00000000-0000-0000-0000-000000000001
nook doctor --deep --timeout 30s
nook doctor --ca-cert ./caddy-local-ca.pem
nook --caddy-socket /run/caddy/admin.socket doctor
```

The hypothetical command after `--` is never executed. Launch options use the
same resolvers as `run`, including workspace v2 validation and existing
worktree/Tailscale restrictions. `--owner` selects a recorded UUID and cannot be
combined with launch simulation options. Without a project, doctor inspects the
machine normally. Resources which have only been configured, not launched, are
not reported as missing Caddy routes.

Configuration precedence remains defaults/global, project, local overlay, CLI.
With an explicit `--config`, its adjacent local overlay is ignored unless
`--local` is given. Workspace members retain their existing merging and naming
rules. Reports show effective values and their provenance, without command
arguments or environment values. `--only` does not bypass validation of the rest
of the workspace.

Linux uses the existing XDG paths; Windows uses APPDATA/LOCALAPPDATA, with the
existing XDG overrides still honored. Unix Admin sockets are Linux-only;
Windows uses an HTTP(S) Admin endpoint. No macOS support is introduced.

## Inspection and active probes

Quick inspection does not create state directories, locks, caches or sessions.
It does not check for updates, join a runner, reserve ports, signal applications,
announce mDNS services, install trust, or invoke any reconciliation routine.
Existing locks are acquired nonblockingly only for the short registry snapshot.
Network observations run after releasing those locks.

Doctor reads Caddy configuration, **not** its local CA endpoint. Both
`GET /pki/ca/local` and its `/certificates` variant can provision a missing CA.
Supplying `--ca-cert` reads an existing public PEM instead. Finding that
certificate in an inspected trust store does not establish that it is Caddy's
active root, or that every browser trusts it.

`--deep` explicitly permits DNS, TCP, TLS, HTTP HEAD and mDNS query traffic.
It still never changes Nook/Caddy configuration, but traffic can be logged or
acted on by the application. A TLS handshake can cause Caddy certificate
issuance or maintenance. Deep mode is therefore not a promise of zero effects
inside the contacted services. Read access can also update OS access metadata.

HTTP probes use `HEAD /`, never fall back to GET, never follow redirects and
send no credentials or cookies. Direct IPv4/IPv6 loopback probes preserve Host
and SNI. An unavailable unused address family is a warning. A supplied CA can
be used explicitly for probe verification; this is distinct from OS trust.
Without it, Linux probes use known system PEM bundles (ureq roots if none are
available); Windows uses native TLS trust. Browser stores may differ.

Readiness recorded by Nook is TCP readiness. An HTTP response does not prove
application health or that the intended Caddy handler produced it. An HTTP
401/403/404/405 is not a network outage; 5xx responses require interpretation.

## What checks can establish

- Configuration errors and conflicting/ignored layers; no silent replacement
  of an invalid global configuration with defaults for Caddy access.
- Registry format, pending recovery, owner identities, process reuse, workspace
  supervisors and original session contexts. Session secrets are not output.
- Required HTTP/HTTPS server selection, Nook containers and route payloads:
  owner, host, upstream, TLS upstream, Host forwarding and client ranges.
- Foreign exact-host collisions, preceding catch-all routes and possible
  wildcard overlap. Arbitrary Caddy matchers are not fully interpreted.
- Bind interfaces and the relationship between `run_bind_address`,
  `caddy_loopback_host` and `caddy_client_ip_ranges`.
- Opt-in Tailscale version, daemon/backend, HTTPS, identity and Serve/Funnel
  registrations, using consultation commands only.
- LAN IPv4 interfaces/listeners and, in deep mode, local mDNS observations.

A readable API does not prove permission to mutate it. File write permission
is not tested by creating files. A lock file's existence does not prove its
lock is held. Never delete a busy lock as a repair.

State and Caddy are not one atomic snapshot. Doctor compares the final state
and configuration with the initial observations; a concurrent change is
reported as incomplete. A missing or unreadable registry cannot authorize
deletion of routes. A route outside an `--owner` selection may belong to another
worktree. Doctor never takes ownership from a matching hostname alone.

## Network viewpoints

Docker support remains an existing-Caddy deployment model, not Docker lifecycle
management. Doctor never contacts the Docker socket or runs `docker exec`.
Linux bridge binds must refer to an interface actually present. Docker Desktop
uses `host.docker.internal` from the container and may require binding the app
beyond Windows loopback. Keep firewall protection enabled. A successful probe
from Nook does not prove the same upstream is reachable from the container.

`.localhost` is reserved for loopback. A failed system lookup does not prove
that a browser's resolver fails; no hosts-file workaround is performed. `.local`
is multicast LAN discovery, not another customizable TLD. Local mDNS success
does not prove another device can cross its firewall or Wi-Fi isolation.

Tailscale Serve is private; Funnel is public and opt-in. Registrations are
observed without `serve off`, reset, login or activation. A local inspection
cannot prove reachability from another tailnet device or the public internet.
Native workspace manifests still reject managed Tailscale exposure, as `run`
does today; external tunnels to Caddy are not managed by doctor.

Compose orchestration and additional tunnel providers are future extensions,
not existing diagnostic capabilities. No daemon manager, framework integration,
custom TLD or privileged repair is added.

## Output contract

Each check has a stable `id`, `resource`, `status`, `reason_code`, human message,
bounded evidence and optional suggested remediations. Outcomes are `pass`,
`fail`, `warning`, `skipped`. Independent checks continue after failure;
unavailable prerequisites are not treated as success.

`--json` writes exactly one report to stdout, with no update banner. The
[JSON schema](doctor.schema.json) documents version 1. Consumers should ignore
unknown fields and branch on IDs/reason codes, not human messages. Breaking
changes require a schema-version increment. The [MCP server](MCP.md) exposes its own observation tools and response contract.
It does not expose `doctor` as a tool; agents can execute this command and parse
its versioned report separately.

| Exit | Meaning |
| --- | --- |
| 0 | No demonstrated failure; warnings and expected skips may remain |
| 1 | At least one failed check |
| 2 | Invalid doctor CLI arguments |
| 3 | Incomplete observation without an already demonstrated failure |

Failure takes precedence over incompleteness. Missing optional CA input or
skipping traffic in quick mode does not make a report incomplete. JSON usage
errors also use the versioned envelope; `--help` remains ordinary text.

Quick mode has a 10-second budget; deep mode has 30 seconds. `--timeout 1s..300s`
changes the total. Admin reads and DNS are capped at 2 seconds, individual TCP
connects at 500 ms, HTTP at 3 seconds, Git/Tailscale commands at 3 seconds, and
mDNS at 2 seconds. Operations run sequentially and use the remaining budget.
Blocking DNS/TLS observations are isolated in terminable workers. Caddy output
is capped at 16 MiB, external-command output at 1 MiB. HTTP bodies are not read.
There are no automatic network retries other than the final consistency read.

Suggested actions are never executed. `nook prune` is global and may clean dead
resources; review ownership first. Do not use `--force` for foreign routes.
CA export/import and Caddy listener changes remain explicit user actions.

## References

- [Caddy Admin API, PKI provisioning and ETags](https://caddyserver.com/docs/api)
- [Caddy automatic and on-demand HTTPS](https://caddyserver.com/docs/automatic-https)
- [Special-use localhost names, RFC 6761](https://www.rfc-editor.org/rfc/rfc6761.html)
- [Multicast DNS, RFC 6762](https://www.rfc-editor.org/rfc/rfc6762.html)
- [Docker Desktop networking](https://docs.docker.com/desktop/features/networking/networking-how-tos/)
- [Tailscale Serve CLI](https://tailscale.com/docs/reference/tailscale-cli/serve)
- [Tailscale Funnel CLI](https://tailscale.com/docs/reference/tailscale-cli/funnel)
