# Docker Compose applications

`nook compose` starts application stacks using the installed `docker compose` CLI
and publishes **explicitly declared HTTP endpoints** through your existing Caddy.
Configure Caddy and trust its CA once, as for `nook run`. Nook never starts,
stops, installs or rewrites the configuration of the Caddy service itself.

This is separate from [running Caddy in Docker](DOCKER.md). There is no Nook
daemon or Docker SDK, and Nook does not open the Docker socket itself. The Docker
CLI still requires permission to use the local engine. The lifecycle is tested with
Compose 2.26.1 and 5.6.1; canonical JSON and, when requested, `--wait-timeout`
are required capabilities. Remote engines, Swarm,
Podman, macOS and Windows containers are not supported.

## First application

```yaml
services:
  web:
    build: .
    ports:
      - target: 3000
        host_ip: "127.0.0.1"
        protocol: tcp
        # No published port: Docker allocates one.
    labels:
      io.ntnook.route.web.port: "3000"
      io.ntnook.route.web.name: "web"
```

```sh
nook compose up
nook compose up -d
nook list
nook status
nook compose down
```

The application must listen on its container network interface, usually
`0.0.0.0:3000`. Nook does not inject application environment variables or framework
flags. It does not modify the Compose files or publish additional ports.

Each `io.ntnook.route.<id>.port` declares an internal HTTP/TCP port. It must already
be published by Compose. `EXPOSE`, `expose`, port names and `app_protocol` alone
never create routes. Databases, caches and UDP ports are not routed.

Optional fields in the same group:

| Field | Default | Meaning |
|---|---|---|
| `name` | `<service>-<id>` | Route name inside the project namespace |
| `tls` | `true` | HTTPS between the browser and Caddy |
| `preserve-host` | `true` | Forward the requested hostname to the application |

The upstream is HTTP in V1. TLS trust, custom upstream certificates, TCP/UDP
proxying, path routing and load balancing are outside this contract. To expose
two HTTP ports, declare two label groups and two port publications. Unknown
route fields, ambiguous publications, port ranges, replicas other than one and
special `network_mode` values on routed services are rejected.

## Files, profiles and project identity

```sh
nook compose -f compose.yaml -f compose.local.yaml \
  --env-file .env.local --profile admin -p boutique-dev up -d web api
nook compose -p boutique-dev down web
nook compose -p boutique-dev logs --follow web
nook compose -p boutique-dev ps --all
```

Global options belong before the action. `-f`, `--env-file` and `--profile` can be
repeated. `--project-directory` fixes the project root. Compose performs YAML
merging, environment interpolation and canonical normalization through
`config --format json`. Local overrides use Compose itself; there is no second
route configuration in `nook.toml`. Stdin and remote Compose files are not supported.

Without `-p`, Nook appends `-nook-<directory-hash>` to Compose's resolved base name
and prints the effective project name. An explicit `-p` is used verbatim, but
cannot take over another registered directory/worktree or an existing unowned
stack. Profiles, selected services and changes to file contents do not change
the project identity. Use distinct `-p` names for distinct stacks in one directory.

Domains are `<route>.<project>.localhost`. In a Git checkout, worktree isolation
is enabled by default and uses the existing Nook worktree identity to produce
`<route>.<worktree>.<project>.localhost`. `--worktree=label` customizes that label;
`--no-worktree` disables the DNS qualification, not resource ownership checks.
The behavior of `nook run` is unchanged. No hosts file or custom TLD is used.

`container_name` and global names for internal networks/volumes are rejected.
Declare deliberately shared networks/volumes `external: true`. Nook labels its
own resources through a generated ownership-only override, retained only for
the duration of the command. It never stores the normalized model or environment
values in its registry. `nook compose config`, like Docker's command, deliberately
prints configuration that may contain secrets.

## Ports and Caddy networking

For a fixed port, add `published: "18080"`. A collision fails; Nook does not fall
back to a different port. For dynamic ports, omit `published`. Nook inspects the
actual binding after startup/recreation, keeping the route UUID stable.

| Setup | Application publication | Caddy upstream |
|---|---|---|
| Linux, native Caddy | `127.0.0.1` | Loopback and published port |
| Windows + Docker Desktop, native Caddy | `127.0.0.1` | Windows loopback and published port |
| Linux, containerized Caddy | Explicit reachable host gateway | Gateway and published port |
| Windows + Docker Desktop, containerized Caddy | Host publication reachable from Desktop | `host.docker.internal` and published port |

A loopback-only publication must not be assumed reachable from containerized
Caddy. For the Linux Caddy network documented in `DOCKER.md`, an application
override can publish on its existing gateway:

```yaml
services:
  web:
    ports:
      - target: 3000
        host_ip: "172.30.0.1"
        protocol: tcp
```

Keep the base publication out of this topology-specific file: Compose may append
publications when overrides use different addresses. Nook rejects ambiguous
results instead of picking one silently.

On Windows Desktop with containerized Caddy, use the existing
`caddy_loopback_host = "host.docker.internal"` configuration. A wildcard
publication may be necessary; that also exposes the application port beyond
loopback. Keep Windows Firewall enabled and verify the route on your Desktop
installation. Nook never opens firewall rules, joins Caddy to application networks
or edits Caddy's `extra_hosts`. Native Caddy is the primary Windows path.

## Lifecycle and recovery

`up` runs attached Compose, forwards logs/signals and observes the application
while attached. Ctrl+C stops the selected services, including selected
dependencies, without removing volumes. `up -d` exits after initial HTTP
readiness and leaves no Nook process running. `--wait` implies detached mode and
also uses Compose's health waiting; `--wait-timeout` defaults to 60 seconds.
Without `--wait`, detached HTTP readiness is bounded to 30 seconds after Compose
returns. A timeout leaves containers available for diagnosis and returns failure.

An initial route requires a running container, a healthy healthcheck if present,
and a `GET /` HTTP response from 2xx through 4xx. Redirects are not followed.
An application requiring a different health path should expose a usable root or
use manual aliases. A temporary HTTP failure does not remove an already installed
route whose owned container and publication still exist.

```sh
nook compose up -d --build --force-recreate web
nook compose up -d --no-deps web
nook compose down web
nook compose down
nook prune
```

Partial up preserves sibling routes; Compose handles dependencies. Partial down
removes only selected owned containers/routes and leaves dependencies and networks.
Full down also removes unused owned networks. **All volumes and images remain.**
This intentionally narrower teardown uses Docker IDs, not an unrestricted
`docker compose down`. It can recover a registered project even after its files
are deleted: invoke it from the registered root, with the effective `-p` if needed.

The registry records route intent before mutation. `status`, `prune` and ordinary
Nook reconciliation discover changed publications, restore missing routes and
withdraw routes for stopped/missing services. They never delete Docker resources
or resume a destructive teardown. Retry `compose down` explicitly to finish one.
When Caddy is unavailable, down still stops owned containers and leaves route
cleanup pending. When Docker is unavailable or its engine identity changed,
Nook preserves ownership evidence instead of assuming the stack disappeared.

An external recreation is recognized only if it retains Nook ownership labels.
An unowned replacement is never adopted or stopped. A foreign container carrying
the same Compose project name blocks up/down. Manual `alias --force` and
`run --force` cannot replace Compose routes. Remove a service with Nook down before
changing an existing route's name or TLS mode.

There is no background observer after `up -d`: external stop/recreate/reload
changes converge at the next Nook command. In that interval a route can be stale,
including pointing to a reused host port. Prefer Nook commands for lifecycle
changes. Concurrent external mutation of the same Compose project is unsupported.

`ps`, `logs` and `config` are the only passthrough actions. Unsupported actions and
flags are errors, including `--scale`, `--watch`, `--remove-orphans`, volume/image
deletion and lifecycle hooks/providers. Use Docker directly for advanced actions,
then reconcile, retaining the ownership limitations above.

## caddy-docker-proxy

Nook never emits `caddy.*` labels. The plugin may reload its generated configuration
after Docker events and remove routes written through Caddy's API. Nook restores
its routes at reconciliation without touching foreign routes, but cannot promise
uninterrupted routing while both controllers write configuration. The official
Caddy image is recommended. A Caddyfile include is not a persistent ownership
mechanism for these dynamic routes.

## State and tests

The first registry write migrates versions 1/2 to version 3 and saves the original
as `state.pre-compose.json`. Older binaries cannot read version 3. Do not restore
an old registry while managed Compose resources are running; stop them first.
Ownership labels alone are not sufficient to adopt resources after losing state.

```sh
cargo test --locked --bin nook -- --test-threads=1
cargo build --locked
NOOK_COMPOSE_E2E=1 python3 tests/compose_e2e.py
# Linux bridge-Caddy alternative, with no privileged host listener:
NOOK_COMPOSE_E2E=1 NOOK_TEST_CADDY_DOCKER=1 python3 tests/compose_e2e.py
```

The real E2E uses unique projects and isolated Caddy/state, and destroys only its
own fixture resources. Native Caddy needs permission to bind its test HTTP port.
The bridge-Caddy variant uses high host ports. Windows unit/CLI checks and native
Caddy CI do not substitute for running the real Compose test on Docker Desktop.
