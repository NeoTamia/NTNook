# Workspaces, runners and worktrees

## Native workspace

For two project servers that already honor `HOST`/`PORT`, put this at the root:

```toml
format_version = 2
name = "shop"
worktree = true

[[apps]]
id = "api"
dir = "apps/api"
command = ["node", "server.js"]

[[apps]]
id = "web"
dir = "apps/web"
command = ["node", "server.js"]

[apps.env]
API_URL = "${api.upstream}"
PUBLIC_API_URL = "${api.url}"
```

```sh
nook run
nook run --only api
```

Git is required by `worktree = true`; omit it when no worktree isolation is
needed. `nook init --workspace --print` previews a starting manifest.
Member directories stay inside the root. Member `nook.toml` values override the
root app entry, then applicable local overrides and CLI options take precedence.
An explicit `--config` still requires `--local` to load local overrides.

Nook prepares ports, routes and references before launching any app. Use
`${api.upstream}` for local server-to-server requests, `${api.url}` for the
browser-facing local URL, and `${api.lan_url}` only when API LAN mode is enabled.
`${api.host}` and `${api.port}` use the actual bind IP and allocated port.
Reserved `PORT`, `HOST`, `NOOK_URL` take precedence over custom env entries.

References must target a selected app: `nook run --only web` fails for this
example because it does not select `api`. Nook does not wait for HTTP dependency
health; keep the application's normal startup retries.

Each app has a prefixed output stream and an individual lease. `nook list`
includes a common `group=<UUID>`. The first member exit, including exit zero,
ends the native workspace and stops its remaining apps. Partial startup failure
cleans up already-started children. Ctrl-C stops the workspace.

## Existing runner

Keep a working runner and its graph instead of replacing them with native mode:

```sh
nook run --runner -- pnpm dev
```

Declare app IDs/directories and Nook settings in the v2 manifest. `command`
entries may be omitted in runner mode. Each participating HTTP package's script
uses `nook-run -- <raw server command>` (or `nook run`); library watchers retain
their scripts. Do not wrap a script that already wraps Nook a second time.

Nook prepares the apps, starts the root command once, and passes
`NOOK_WORKSPACE_SESSION`. Each wrapper joins by its package working directory.
The root owns names and settings; conflicting member port/bind/TLS/exposure flags
fail. Do not fabricate, copy between runs, or clear a session marker to bypass
a session error. The server itself does not need the marker.

Runner filtering stays in the runner. `--runner` conflicts with Nook's `--only`.
The runner owns output, dependency order and failure policy: a failed member may
leave siblings alive. While the root session lives, a member can restart using
its prepared port. Root exit stops remaining owned children and removes routes.
Shutdown has a grace period; serialized member cleanup can take more than two
seconds overall. Keep the runner session handle until it has really exited.

Turbo strict mode must pass the session marker. Preserve existing config and
add it to an appropriate allowlist, for example:

```json
{"globalPassThroughEnv":["NOOK_WORKSPACE_SESSION"]}
```

Keep persistent tasks uncached and allow enough parallel tasks. Do not switch
the entire project to loose environment mode. Nook's Turbo warning inspects
root configuration, not the entire task graph. See
[Turbo's environment contract](https://turborepo.dev/docs/crafting-your-repository/using-environment-variables)
and the [tested runner recipes](https://github.com/NeoTamia/NTNook/blob/a2a4ce5e6e65145dfc880c626e2903a1211d38d3/docs/WORKSPACES.md#existing-workspace-runner-scripts)
only for the project's actual runner. Those recipes distinguish pnpm, Bun,
Turbo, Lerna, Nx, Vite Task, and bounded Yarn/moon coverage.

## Concurrent Git worktrees

```sh
nook run --worktree
nook run --worktree=agent-a
nook run --worktree=agent-a --runner -- pnpm dev
```

Prefer automatic identity for concurrent Git copies. Choose a distinct explicit
label per copy when reproducible URLs are needed. The optional value uses `=`.
Keep a shared project name such as `shop`; the example above with `agent-a`
produces `api.agent-a.shop.localhost` and `web.agent-a.shop.localhost`. Confirm
the actual URLs in output, especially with custom member names.

Automatic identity uses the worktree directory and a hash of its canonical Git
administrative path. Branch switches do not change it; moves can. Stop old runs
before moving a worktree. `--worktree` overrides a configured label with automatic
identity, and `--no-worktree` disables isolation. Default is disabled.

Worktree is a root v2 setting, not an `[[apps]]` setting. A member's own setting
applies when launched standalone; standalone launch does not discover a parent
workspace. Nook creates no Git worktrees and isolates no databases or files.

Every launch retains its own UUID ownership. A live hostname owned by another
worktree is rejected even with `--force`. Stop by the complete observed hostname
or the root session; leave other worktrees running. `prune` is shared user-wide
reconciliation, not a worktree selector. See [recovery](recovery.md).
