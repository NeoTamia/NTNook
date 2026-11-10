# Diagnose and recover owned resources

## Locate the failure

Start with captured stderr, `nook config show`, and the relevant command's
`--help`. If Caddy is reachable, `nook status` diagnoses state and `nook list`
identifies runs. Both can reconcile shared state; they are not read-only probes.

| Symptom | Next action |
| --- | --- |
| Admin API unavailable | Verify address and socket permissions on Linux, or TCP URL on Windows. Report service unavailability; do not restart Caddy automatically. |
| Missing/ambiguous listener | Use reported candidates; existing `https-server` / `http-server` config keys select an existing listener. Listener creation belongs to machine preparation. |
| Local CA not trusted | Report Nook's exact trust remediation for manual setup. `nook ca export caddy-local-ca.pem` exports a public certificate but does not install it. |
| Strict port occupied | Choose another port or remove the fixed-port requirement. Preserve the unrelated occupant. |
| Foreign route collision | Choose another Nook name; preserve the foreign route. |
| Nook hostname collision | Identify the owner/worktree before deciding to replace or stop it. Do not automatically add `--force`. |
| URL announced but no HTTP | Check the actual bind/port, raw child command, logs, dependency startup, and application response. |
| Runner member lacks its environment | Check wrapper participation, package cwd and runner env filtering. Do not remove the session marker to bypass a failure. |
| Drift or pending cleanup | Restore dependency access, then use `nook prune` and inspect the result. |

One app preserves its child exit status even when route cleanup is deferred.
Native workspaces stop siblings on the first member exit; runners retain their
own failure policy until the root exits. Report both the launch outcome and any
remaining cleanup warning rather than rerunning a failing command in a loop.

## Stop the resources you own

Prefer Ctrl-C on the tracked root session for a whole workspace. For a specific
managed run, use its complete observed hostname, for example:

```sh
nook stop web.localhost
```

Wait for termination; `stop` reports a signal sent, not completed shutdown.
Stopping a native workspace member also ends that native workspace. A runner
can restart a member while its root session lives; stop the root when the task
is to finish the entire session.

`nook stop <hostname> --force` forcibly terminates an identified managed run
when ordinary shutdown failed. It is different from `nook run --force`, which
transfers a Nook-owned route without stopping the former process. Cross-worktree
live ownership cannot be overridden with `run --force`.

If you created a persistent alias named `docs` for this task, remove that alias:

```sh
nook alias remove docs
```

After a crash or deferred cleanup:

```sh
nook prune
```

Reconciliation is user-wide and may restore live owned routes as well as remove
stale ones. It preserves unrelated live runs and foreign routes. Check pending
warnings, then verify your run is gone and known neighbors still respond.
If Caddy is unavailable, report pending cleanup and retry after access returns.
Do not delete the registry, clear all routes, kill by a broad process name, or
reset an external service. Caddy's API has optimistic concurrency, not a
multi-request transaction; let Nook maintain its ownership and recovery journal.

## Existing optional exposure

Keep local exposure unless the task or existing project configuration calls for
more. `--no-lan` disables configured LAN; `--no-tailscale` disables both Serve
and Funnel. Neither resets an explicitly configured application bind address.

- **LAN:** `--lan` adds `http://<name>.local` and mDNS, requiring a suitable Caddy
  port 80 listener. The app remains loopback-bound by default. Binding
  `0.0.0.0` additionally exposes its direct port and is a separate decision.
- **Serve:** existing `--tailscale` exposes standalone runs/aliases privately
  through Tailscale. It is rejected for multi-app workspaces.
- **Funnel:** existing `--funnel` is public internet exposure. It is rejected
  whenever worktree isolation is enabled and requires existing authorization.
- **External tunnel to Caddy:** configured outside Nook. Preserve the selected
  app Host/routing key; `.localhost` resolves at the client and `.local` mDNS
  does not automatically cross the tunnel. Do not create DNS/tunnel endpoints.

For existing Tailscale exposure, `nook tailscale status` diagnoses registrations
and reconciles Tailscale without requiring Caddy. An unavailable daemon keeps
work pending. A changed device identity can defer recovery; a foreign replacement
must be preserved. Never run a blanket Serve reset to fix an owned registration.
Consult the [existing exposure contract](https://github.com/NeoTamia/NTNook/blob/a2a4ce5e6e65145dfc880c626e2903a1211d38d3/README.md#tailscale)
only when needed.
