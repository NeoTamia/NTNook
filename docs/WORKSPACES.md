# Multi-app workspaces

A version 2 `nook.toml` describes several apps. Each app keeps its own domain,
port, working directory and process. A version 1 member configuration continues
to work when that app is started on its own.

```toml
format_version = 2
name = "shop"

[[apps]]
id = "api"
dir = "apps/api"
command = ["bun", "--watch", "src/index.ts"]

[[apps]]
id = "web"
dir = "apps/web"
command = ["nuxt", "dev"]

[apps.env]
NUXT_BACKEND_URL = "${api.upstream}"
```

Run `nook run` from the root, or select a member with `nook run --only api`.
`nook init --workspace` generates a starting configuration. Commands are argv
arrays: Nook does not evaluate shell expressions or search package scripts.
Use raw server commands in the native mode above, rather than commands which
start another `nook-run`.

Environment references must name a selected app. For example, `--only web` fails
if `web` references `api`, since that invocation does not start an API instance.

Member `nook.toml` values override the root app entry. Member local overrides
then take precedence, followed by CLI options. An explicit `--config` keeps the
existing opt-in `--local` rule. App IDs, directories and resolved domains must
be unambiguous. Member directories stay inside the workspace root.

Before starting any app, Nook reserves the ports, installs the routes and
resolves environment references:

| Reference | Value |
| --- | --- |
| `${api.url}` | Public route, for example `https://api.shop.localhost` |
| `${api.upstream}` | Actual application address, for example `http://127.0.0.1:41001` |
| `${api.host}` | Application bind IP, for example `127.0.0.1` |
| `${api.port}` | Actual allocated port, including any fallback |

Each child receives its own `PORT`, `HOST` and `NOOK_URL`. These reserved
variables take precedence over custom environment entries. Unknown references
fail before starting children. URL availability does not imply that an app is
ready to serve requests; apps should retain their normal startup retry logic.

Output is prefixed by app ID. Ctrl-C stops the workspace and removes its
managed routes. A startup failure stops already-started children and cleans up.
Individual members remain visible through `nook list` and can be stopped by
hostname. There is no permanent workspace daemon.

## Existing Turbo and Lerna scripts

The runner mode retains the package scripts already using `nook-run`:

```json
{
  "scripts": {
    "dev": "nook-run -- nuxt dev"
  }
}
```

Keep the root script as `turbo run dev` or `lerna run dev`, then invoke it through
the workspace:

```sh
nook run --runner -- pnpm dev
```

In runner mode the `command` fields in `[[apps]]` may be omitted: the existing
package scripts supply the commands. Declare only each app's `id`, `dir` and
Nook settings, plus any environment references. Native `nook run` still requires
a command for every member.

In this mode Nook prepares the declared apps, then starts the root command once.
The runner owns its task graph, package scripts, output and non-HTTP tasks.
Nook does not start the app commands a second time. Each package's `nook-run`
invokes the system Nook binary, which reads `NOOK_WORKSPACE_SESSION`, matches the
package's working directory to its declared app, and applies that app's resolved
environment. Without a session, the wrapper retains its standalone behavior.
A supplied but expired or invalid session fails explicitly instead of starting
an unrelated standalone run.

An existing wrapper `--name` is accepted; the declared app directory selects the
session member and the root owns its domain. Conflicting port, bind, TLS or
exposure options fail explicitly. Move those settings into the workspace/member
configuration so Nook can plan them before the runner starts.

Only apps declared in `[[apps]]` get routes. Other tasks, such as library watchers,
keep their existing scripts. All participating HTTP app scripts must use
`nook-run` (or `nook run`) to receive their own environment. An arbitrary root
command cannot distribute different `PORT` values to unmodified child servers.

### Turborepo

Turbo's default strict environment mode filters variables before invoking package
scripts. Allow the session marker explicitly; Nook does not switch Turbo to loose
mode or rewrite `turbo.json`:

```json
{
  "globalPassThroughEnv": ["NOOK_WORKSPACE_SESSION"],
  "tasks": {
    "dev": {
      "cache": false,
      "persistent": true
    }
  }
}
```

Keep the project's existing dependencies and task settings alongside this
configuration. Nook applies per-app ports and URLs after the wrapper starts, so
they are not a shared root `PORT`.

### Lerna

Lerna runs the existing package `dev` scripts. Configure enough concurrency for
the selected long-running tasks and use the project's normal Nx task pipeline.
Nook does not alter dependency order, `.env` files or caching settings.

Root runner filters select tasks in that runner; they are not automatically
translated from Nook's native `--only` option. Packages still work independently
when run outside a workspace session. Other runners can use the same inherited
session mechanism, but require their own compatibility validation.

Official references:

- [Turbo environment variables](https://turborepo.dev/docs/crafting-your-repository/using-environment-variables)
- [Turbo task configuration](https://turborepo.dev/docs/reference/configuration)
- [Lerna tasks](https://lerna.js.org/docs/features/run-tasks)
- [Lerna task pipelines](https://lerna.js.org/docs/concepts/task-pipeline-configuration)

## Validation

The normal Rust test suite covers configuration, interpolation, native launches
and cleanup. The explicit runner gate exercises real Turbo and Lerna with two
HTTP apps using the repository's Node wrapper and one non-HTTP package:

```sh
cargo build --locked
npm install --prefix target/workspace-runner-tools --no-audit --no-fund turbo@2.11.10 lerna@10.1.1
node --test tests/workspace_runners.mjs
```

This gate requires Node 22+ and Caddy 2.11.x. On Linux it uses `unshare` and `ip`
to isolate port 80 in a private network namespace. Set `NOOK_RUNNER_DIRECT=1`
only when running with a free, permitted loopback HTTP port 80. It uses temporary
project, registry and Caddy data directories and installs no CA.

Worktree-specific domain isolation is tracked separately in NOOK-74, after the
monorepo work. It is not inferred from branch names by this workspace format.
