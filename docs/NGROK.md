# ngrok tunnels

Nook can expose a run through ngrok while keeping its existing local Caddy route.
Install and authenticate ngrok separately, then use:

```sh
nook run --tunnel ngrok -- vite
nook run --tunnel ngrok --ngrok-url https://demo.ngrok.app -- npm run dev
nook run --no-tunnel
```

The qualified agent is **ngrok 3.39.13** on Linux. Windows uses the same CLI
contract with native process ownership tests. Nook neither installs ngrok nor
modifies its account configuration, domains, services or DNS. There is no macOS
support. Public access is opt-in: anyone with the URL can reach the exposed app.

## Configuration and URLs

```toml
format_version = 1
name = "web"
command = ["vite"]
tunnel = "ngrok"
# Optional: an ngrok domain already available to your account.
ngrok_url = "https://demo.ngrok.app"
```

Put personal settings in `nook.local.toml`. Existing file precedence and
`--config` / `--local` rules apply. `--no-tunnel` or `tunnel = "none"` disables
exposure. CLI selection takes precedence; `ngrok_url` is ignored while another
provider is selected, but an explicit `--ngrok-url` requires ngrok. `nook init
--tunnel ngrok [--ngrok-url URL]` generates these settings.

Optional machine settings in `nook config path`:

```toml
[ngrok]
binary = "ngrok"
# config = "/path/to/ngrok.yml"
startup_timeout_seconds = 30
```

Windows accepts a TOML literal path such as `'C:\Tools\ngrok.exe'`. Without an
explicit config path, Nook uses `~/.config/ngrok/ngrok.yml` on Linux or
`%LocalAppData%\ngrok\ngrok.yml` on Windows. Alternatively use
`NGROK_AUTHTOKEN` in the invoking shell. Do not store tokens in a project file.
Nook validates files with `ngrok config check`, and adds a private v3 overlay.
The qualified agent accepts an existing v2 credential file with this overlay;
Nook does not migrate it. Commas in configuration/session paths are refused
because the agent's multi-file `--config` uses comma separation.

`NOOK_URL` stays local. The shared Cloudflare/ngrok variable `NOOK_TUNNEL_URL`
and `{tunnel_url}` command placeholder contain the public HTTPS origin before
application spawn. Existing Vite-based allowed-host handling adds the exact
public hostname, never a provider-wide wildcard. There is no new framework
integration. Explicit HMR configuration may still be necessary for apps that
hardcode a hostname: use `wss` and external port 443, without breaking local use.

Without an explicit URL, ngrok chooses the endpoint according to the account's
configuration and plan. Nook discovers the actual URL from the agent API and
does not promise that it stays unchanged across launches. For a stable URL, use
`--ngrok-url` with a domain already available to your account. Separate worktrees
do not receive additional free domains. Nook supports ngrok domains only;
personal domains, provisioning and DNS management
are outside this version. URLs cannot contain credentials, paths, queries,
fragments, wildcards or nonstandard ports.

[Current free-plan limits](https://ngrok.com/docs/pricing-limits/free-plan-limits)
include one development domain, up to three online endpoints and three agents,
plus request and bandwidth quotas. These are provider policies, not guarantees
or hardcoded limits in Nook; see [pricing](https://ngrok.com/pricing). A browser
interstitial may appear on the free plan. API clients and webhooks have different
behavior; the client-side `ngrok-skip-browser-warning` header can bypass the
interstitial for tests. Nook does not inject it into public visitors' requests.

## Traffic and TLS

Public traffic goes directly from the local ngrok agent to the app's loopback
upstream. Caddy still serves local/LAN traffic, but **Caddy middleware does not
apply to the public ngrok path**. There is no new technical Caddy route and no
change to foreign routes, listeners or CA setup. Docker's `caddy_loopback_host`
is not used by ngrok; the agent runs alongside Nook and reaches the host app.
Wildcard bind addresses become loopback addresses of the same IP family.

Nook generates a small Traffic Policy to set Host to the upstream authority,
matching the normal local alias/run behavior. For `alias --preserve-host`, Host
uses that alias's `.localhost` name. ngrok's forwarded headers carry public
request information; configure application proxy trust deliberately. Nook does
not rewrite Origin, cookies, CORS, signatures or response bodies. ngrok itself
may adapt Location when rewriting Host.

The public certificate belongs to ngrok's HTTPS endpoint. For HTTPS upstreams,
Nook always passes `--upstream-tls-verify=true`; there is no insecure fallback.
Upstream URL controls TLS identity/SNI independently of HTTP Host. ngrok URL
readiness does not probe that upstream's TLS: a request to an untrusted origin
fails at the proxy. The opt-in live test verifies both rejection and explicit CA
trust. [Official HTTP/S behavior](https://ngrok.com/docs/gateway/endpoints/http).

WebSockets and Vite HMR are covered by a real ngrok test. Webhooks preserve binary
bodies and signature headers; apps signing complete URLs must use the public
origin. Nook makes no application requests during readiness, avoiding accidental
business side effects. ngrok request inspection is disabled for its owned agent.

## Workspaces and worktrees

```toml
format_version = 2
name = "shop"
worktree = true

[[apps]]
id = "api"
dir = "apps/api"
command = ["npm", "run", "dev"]
tunnel = "ngrok"
ngrok_url = "https://shop-api.ngrok.app"

[[apps]]
id = "web"
dir = "apps/web"
command = ["vite"]
tunnel = "ngrok"
ngrok_url = "https://shop-web.ngrok.app"

[apps.env]
VITE_API_URL = "${api.tunnel_url}"
```

Each exposed app owns one agent and endpoint. More than one selected ngrok app
requires explicit distinct URLs. `--only api` can use the implicit dev domain;
root `--ngrok-url` cannot supply one URL for multiple apps. No pooling is enabled.
An account with only its dev domain cannot obtain two separate app URLs this way.

Native workspaces and `nook run --runner -- pnpm dev` resolve all URLs before
launching any app/runner. Joining members inherit the prepared URL and never
launch an agent. A member restart retains its endpoint until the root or member
is explicitly stopped. Separate worktrees keep separate owners and ports; use
different reserved URLs in their local configuration. `--force` never takes an
endpoint from another owner or foreign ngrok process.

LAN and existing single-run Serve/Funnel options retain the Cloudflare base's
compatibility rules. Workspaces still reject managed Serve/Funnel; Funnel still
rejects worktree mode. Selecting ngrok replaces Cloudflare for that app.

## Foreground aliases

```sh
nook alias set api 3000
nook tunnel alias api --provider ngrok
nook tunnel alias api --ngrok-url https://api-demo.ngrok.app
nook tunnel list
nook tunnel stop <tunnel-uuid>
```

The alias remains persistent; its tunnel lasts only while the foreground command
runs. Ctrl+C closes exposure. Removing or replacing the alias withdraws its old
tunnel, never repoints it. Only loopback HTTP(S) upstreams are supported in V1.
For a private HTTPS origin, pass `--upstream-ca /path/to/public-root.pem`. The
certificate is not installed in the system trust store; private keys are unused.
`nook tunnel stop` is for alias sessions. Runs stop with `nook stop <name>`.

## Ownership, failures and recovery

- API discovery uses `/api/endpoints` on a dedicated ephemeral loopback port,
  matching the random session name and exact upstream. URLs are not scraped from
  logs. The API is unauthenticated; never expose it to LAN/public clients.
- Nook never attaches to another agent's API or starts user-configured endpoints.
  An instance already on 4040 remains untouched. Bind failures retry at most
  three times. Domain/quota conflicts fail without pooling or killing strangers.
- Initial connection/discovery failure prevents app spawn. A workspace rolls
  back all prepared exposures. The URL can exist before the app listens; the
  existing TCP readiness warning remains separate.
- After startup, connector death or loss of its endpoint reports failed/degraded
  state while local development stays running, matching Cloudflare. Nook does
  not restart the connector or silently change the injected URL. Alias sessions
  exit nonzero if their connector exits. Restart explicitly to recover.
- App exit, stop and supervisor loss withdraw owned agents using the shared
  process identity/group/Windows Job Object lifecycle. Cleanup is journaled and
  idempotent. `prune` never republishes anything and never revokes a reserved
  ngrok domain. Indeterminate identity cannot authorize killing a process.
- State upgrades from historical formats 1–5 to provider-aware format 6, preserving Compose and all tunnel ownership. See [registry compatibility](CLOUDFLARE.md#failure-and-recovery). Stop active runs before upgrading;
  do not mix old/new binaries against one registry. Old binaries reject format 6.
- Credential variables are removed from app/runner children. Only the ngrok
  child receives `NGROK_AUTHTOKEN`; API keys are not needed. Tokens never enter
  argv or the Nook registry. Output is bounded/drained and never printed raw.
  Private session files contain only configuration, policy and public metadata.

## Verification

The normal Rust suite uses a fake CLI/API and real Caddy; no account is required.
It covers configuration, malformed/foreign endpoints, startup rollback, secrets,
workspace interpolation, crash/prune, alias lifetime and native Windows spawning.
The Cloudflare suite continues to validate the shared lifecycle.

For a real opt-in test in a disposable Linux container, install Caddy 2.11.x,
ngrok 3.39.13, Nook, Python, Node and Vite 8. Do not publish container ports or
serve repository contents. With an authtoken available only to the test:

```sh
NOOK_E2E_NGROK=1 python3 tests/ngrok_live.py
```

`NGROK_TEST_TOKEN_FILE` optionally points at a securely mounted token file. The
script generates its own fixture and verifies public HTTPS, a signed binary
webhook, bidirectional HMR traffic, trusted/untrusted HTTPS aliases and cleanup.
It consumes a few requests on the account and leaves no public endpoint running.
