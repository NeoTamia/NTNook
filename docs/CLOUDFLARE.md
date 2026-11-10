# Cloudflare Quick Tunnels

Nook can publish a temporary HTTPS URL in addition to an application's local
Caddy URL. Install `cloudflared` explicitly, prepare Caddy and its CA as usual,
and run:

```sh
cloudflared --version
nook run --name web --tunnel cloudflare -- vite
```

Linux and Windows x86-64 are supported. The initial compatibility target is
cloudflared **2026.10.7**. Nook checks executable capabilities before launching
applications. It does not download cloudflared, log into Cloudflare, install a
service, provision a domain, or manage Caddy's lifecycle.

## Public access and provider limits

This mode is **public**: anyone with the URL can access the application. Do not
expose production data or a development server containing secrets. Enabling
`tunnel = "cloudflare"` in a project file also enables public access on each run;
use `nook.local.toml` if this should be a personal choice.

Cloudflare's [Quick Tunnel documentation](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/)
(currently checked November 2026) specifies an ephemeral `trycloudflare.com`
hostname, no uptime guarantee, no SSE support, and at most 200 in-flight requests
per Quick Tunnel (additional requests receive HTTP 429). This is not a limit of
200 visitors or 200 tunnels. Nook makes no guarantee about unlimited simultaneous
tunnels. Each exposed app has a separate connector and URL.

WebSocket traffic is supported, including Vite HMR. Applications must reconnect
when a connection is interrupted. SSE-based applications need another exposure
method. Caddy supporting SSE does not remove Cloudflare's Quick Tunnel limit.

Cloudflare now also offers Quick Tunnel email authentication with `--allowed-mail`.
Nook's first integration does **not** enable or configure it. There is no implicit
Access policy. Named tunnels, stable public domains and authentication are future
extensions, with explicit account/domain provisioning and credentials.

## Configuration

```toml
# nook.toml or nook.local.toml
format_version = 1
name = "web"
command = ["vite"]
tunnel = "cloudflare"
```

Use `tunnel = "none"` for an explicit file override. `--tunnel cloudflare` and
`--no-tunnel` override files and conflict with each other. `--config` retains the
existing opt-in `--local` behavior. `nook init --tunnel cloudflare` generates the
setting; `nook init --workspace --tunnel cloudflare` puts it on the sample app.

Optional global configuration, in the file printed by `nook config path`:

```toml
[cloudflare]
binary = "cloudflared"
protocol = "auto"
startup_timeout_seconds = 60
```

`binary` is a PATH executable or a path, not a shell command. On Windows, use
`cloudflared.exe` or a TOML literal path such as `'C:\Tools\cloudflared.exe'`.
`protocol` accepts `auto`, `quic`, or `http2`; the timeout accepts 1–600 seconds.
Global settings never activate exposure on their own.

The firewall must permit provisioning over HTTPS and connector traffic to
Cloudflare on outbound UDP/7844 (QUIC) or TCP/7844 (HTTP2), plus DNS. Use `http2`
explicitly where UDP is blocked. Nook does not change firewall rules.
See [Cloudflare's network requirements](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/configure-tunnels/tunnel-with-firewall/).

## Caddy, TLS and ownership

The connector runs alongside Nook and connects to `https://127.0.0.1:443`.
Caddy must already publish HTTPS there, including when Caddy runs in Docker.
`caddy_loopback_host` continues to mean the address **Caddy uses to reach the
application**; it is not the connector's Caddy address. Nonstandard origin
listeners are not supported by this first integration.

Each exposure owns a technical `tunnel-<uuid>.localhost` route. cloudflared uses
this hostname for both HTTP Host and TLS SNI, while connecting directly to the
loopback IP. No hosts-file or DNS changes are required. Nook exports Caddy's
public local CA into the exposure directory and passes `--origin-ca-pool`.
Certificate chain and hostname verification remain enabled. No private key or
system trust store is changed. The initial TLS probe validates the origin before
spawning an application.

A private PID file, written by cloudflared after its first connection, must match
the owned child before Nook accepts metrics and the public URL. A service found
on a raced metrics port is not sufficient evidence of readiness.

The technical route initially returns 503. Once the connector supplies its URL,
the route proxies the app and sets Host/X-Forwarded-Host to the exact public host
and X-Forwarded-Proto to `https`. These values are not taken from visitor-supplied
forwarding headers. The existing Caddy client IP restrictions are preserved;
Docker deployments must retain their documented client ranges and loopback mapping.
Nook does not configure global trusted proxies or guarantee visitor IP attribution.

Local and LAN routes remain unchanged. `--no-tls` only makes the local URL HTTP;
the connector still requires Caddy HTTPS. Foreign routes, listeners and Caddy's
Admin API configuration remain untouched. `--force` withdraws the old owned
exposure: its URL is never repointed to the replacement app.

## URLs, frameworks and workspaces

`NOOK_URL` remains the local URL. `NOOK_TUNNEL_URL` contains the public URL and is
available **before** application spawn. It is absent on unexposed runs, even when
the parent shell inherited an earlier run's URL. Literal command arguments can
use `{tunnel_url}`, just as they can use `{port}`:

```sh
nook run --tunnel cloudflare -- my-server --public-url '{tunnel_url}'
```

No shell evaluation is performed. A tunnel placeholder without exposure is an
error. The public hostname is added to existing Vite-based allowed hosts; Nook
never enables `allowedHosts = true` or a wildcard for `trycloudflare.com`.
Vite versions must support the additional-host environment format used by their
configuration. Vite 8 is the live smoke-test target; old versions may require an
explicit `server.allowedHosts` array in the project.

HMR normally derives its connection from the browser origin. If an application
hardcodes its HMR address, configure `wss`, the tunnel hostname and client port
443 explicitly using `NOOK_TUNNEL_URL`. Do not globally pin HMR to that address if
the same process must also support local browsing. Nook does not rewrite project
files, cookies, redirects or CORS. Existing framework detection limits still
apply to package-script wrappers. No new framework integration is introduced.

A workspace can publish selected apps:

```toml
format_version = 2
name = "shop"
worktree = true

[[apps]]
id = "api"
dir = "apps/api"
command = ["node", "server.js"]
tunnel = "cloudflare"

[[apps]]
id = "web"
dir = "apps/web"
command = ["vite"]
tunnel = "cloudflare"

[apps.env]
VITE_API_URL = "${api.tunnel_url}"
```

All selected tunnel URLs are obtained before final interpolation and before any
app or runner starts. `${api.url}` remains local and `${api.upstream}` remains an
internal address; a remote browser normally needs `${api.tunnel_url}`. References
to disabled or unselected tunnels fail before spawn. Existing dependency-cycle
validation remains in effect.

`--tunnel cloudflare` at the root enables all selected apps; `--no-tunnel`
disables them. `--only api` selects one app. Connectors start sequentially to bound
provider provisioning and avoid a global lock during network waits. Each has its
own startup deadline. URL discovery is not application readiness: an app can
still be starting after its URL exists.

`nook run --runner -- pnpm dev` prepares the URLs before the runner. Member
`nook-run` wrappers inherit the prepared values and never spawn another connector.
Exposure overrides on joining members are rejected. A runner can restart an app
while retaining its assigned URL; explicitly stopping that app withdraws its
tunnel. Restart the root run to create a replacement tunnel.

Worktrees get independent owners, ports, routes and connectors. LAN can coexist
with Cloudflare. Single runs can also use Tailscale Serve or Funnel under their
existing restrictions. Managed Serve/Funnel remain unsupported in workspaces,
and Funnel remains unavailable in worktree mode. `--no-tunnel`, `--no-lan` and
`--no-tailscale` affect only their own exposure method. Persistent aliases do not
support managed Quick Tunnels.

## Failure and recovery

- Initial URL/connection failure aborts startup; no application is launched.
  Workspace startup rolls back all connectors already prepared.
- A transient network failure after startup marks the tunnel degraded. The app
  keeps running and cloudflared can reconnect with the same URL.
- If the connector exits, Nook reports failure and keeps local development alive.
  Nook does not respawn it: a new Quick Tunnel changes the URL and would invalidate
  the environment already given to applications. Restart the run explicitly.
- App exit, Ctrl-C and `nook stop` withdraw exposure. Connector shutdown is bounded
  to five seconds before forcing its owned process group/Job Object to stop.
- `nook list` and `nook status` show provider, phase and URL. Readiness is the
  connector state, not proof of application health or worldwide DNS propagation.
- `nook prune` removes orphaned connectors/routes and retries deferred cleanup.
  It never creates a new public tunnel. Unknown process identity is not permission
  to terminate a process. A Caddy outage does not prevent connector shutdown.
- A CA rotation requires restarting the run; TLS validation is never disabled as
  a recovery measure.

Per-exposure configuration is private and independent of `~/.cloudflared`.
Nook neither renames personal files nor inherits `TUNNEL_*`/`CLOUDFLARED_*`
configuration variables into the connector. Auto-update is disabled for the
owned process. Connector output is drained with a 64 KiB total memory budget;
raw logs are not printed or persisted because they can contain credentials or
request information. Startup errors instead report actionable categories.

The registry migrates to format 3, adding connector and preparation journals.
Stop existing runs before upgrading, and do not mix old/new Nook binaries against
one registry. Older binaries reject format 3. No automatic downgrade is provided.

## Verification

`cargo test --locked -- --test-threads=1` includes deterministic Linux Caddy/
fake-connector tests and the native Windows connector test in the Windows suite.
Linux uses the repository's isolated network namespace convention for ports 80/443.
The normal suite never opens a public tunnel.

For a real opt-in proof, use a disposable Linux container with ports 443/2019
available internally and no published host ports. Install Caddy 2.11.x,
cloudflared 2026.10.7, Nook, Python, Node and Vite 8 on PATH, then run:

```sh
NOOK_E2E_CLOUDFLARE=1 python3 tests/cloudflare_live.py
NOOK_E2E_CLOUDFLARE=1 NOOK_TEST_TUNNEL_PROTOCOL=http2 python3 tests/cloudflare_live.py
```

The script generates its own temporary web fixture, checks public HTTP and a real
Vite HMR WebSocket update, and stops its processes. It does not expose repository
contents. Provider availability can fail this manual test independently of local
correctness. No public load test is performed.
