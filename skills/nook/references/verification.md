# URL discovery, readiness, E2E and logs

## Find the effective URL

The tracked launch prints information to stderr, for example:

```text
nook: domain=web.localhost url=https://web.localhost port=5173
```

Use that URL, including its scheme. `NOOK_URL` is available inside the child;
workspace env references provide sibling URLs. Neither sets the parent shell's
environment. `nook list` helps correlate domains and workspace `group=<UUID>`;
its text columns vary with exposure and grouping and are not a JSON API.
`list` and `status` may reconcile state and require Caddy.

When the installed `--help` exposes the structured interface, prefer
`nook list --read-only --json` and `nook status --read-only --json` for observation.
Use `nook url --json` with the launch's config, local override and worktree options
to resolve a registered URL; select `--app <id>` for a workspace member. Check
`schema_version` and `ok` before consuming `data`. A `--mode configured` URL is a
prediction, not evidence of a registered route or application readiness. CLI list
returns at most 100 entries; MCP supports pagination for larger registries.

If an MCP connection is already configured, use its advertised read tools and
fixed project/worktree context for discovery. Alias mutation requires explicit
hostname grants. Start and stop application sessions through the shell workflow;
MCP does not launch processes or manage the Caddy service.

Do not discover URLs by reading the private state registry, guessing a worktree
hash, assuming the requested port was free, or adding a port to a Caddy URL.

## Verify the application

Distinguish three stages:

1. Route prepared and URL announced.
2. Upstream TCP accepts a connection: Nook reports `ready`.
3. HTTP returns the application's expected status/content and dependencies work.

Only the third stage is evidence for an application test. Probe a known endpoint
with bounded retries (60 seconds by default, or the project's existing timeout).
Choose the endpoint and expected response from the project, not an invented
`/health` route. `--readiness-warn-after` controls a warning, not a kill timeout;
Nook keeps the process and route active when the warning fires.

On timeout, capture the launch output and effective URL/port, diagnose the
failing layer, and stop your temporary test session. A 404 from the wrong app,
a successful TCP connection, or a wrapper fallback is not a successful E2E.

## Run the project's E2E command

Use the project's existing browser/test tools. The portable default is an
externally supervised Nook session: start once, observe its URL, wait for HTTP,
run tests, then stop the session you created. Reuse an existing session only
when the user intended that exact app/worktree; leave it running afterward.

If the project's test configuration reads `E2E_BASE_URL`, pass the observed URL:

```sh
E2E_BASE_URL=https://web.localhost npm run test:e2e
```

```powershell
$env:E2E_BASE_URL = 'https://web.localhost'
npm run test:e2e
```

`E2E_BASE_URL` is a recipe convention, not a Nook variable. Restore any existing
PowerShell environment value after the test. For an existing Playwright setup,
connect it to `use.baseURL` rather than hard-coding a development port.

An optional Playwright `webServer` can own a Nook launch when the URL is
deterministic. Set `reuseExistingServer: false` for isolated test runs and wait
on the app URL, not merely the announced output. Playwright's URL readiness
accepts several HTTP status codes; assert the app's expected response as well.
Its `gracefulShutdown` option is ignored on Windows, so verify Nook cleanup on
both platforms. See the [official webServer contract](https://playwright.dev/docs/test-webserver).

Keep TLS verification enabled. If the test runtime does not trust the prepared
Caddy CA, diagnose that runtime's trust store. Do not globally disable TLS
validation or silently change the app to HTTP to make a failing HTTPS test pass.

## Keep useful logs

- Standalone: retain the session's stdout/stderr and exit status.
- Native workspace: use the app prefixes to identify the failing member.
- Runner: retain its own stream formatting and root exit status.

There is no `nook logs` command or persisted log service. If artifacts are
needed, capture streams into files owned by this test and preserve the command's
status; a shell pipeline can otherwise report the logger's success instead.
Avoid logging entire environments, secrets, or workspace session files.

Report the URL, the response or test proving readiness, test exit status, and
whether the exact owned session stopped. An exit code is not proof that pending
route cleanup completed; use the [recovery procedure](recovery.md) when needed.
