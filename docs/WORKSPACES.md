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

## Existing workspace runner scripts

The runner mode retains the package scripts already using `nook-run`:

```json
{
  "scripts": {
    "dev": "nook-run -- nuxt dev"
  }
}
```

Keep the root development script and invoke it through the workspace:

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

Root runner filters select tasks in that runner; they are not automatically
translated from Nook's native `--only` option. Packages still work independently
when run outside a workspace session. Nook does not change the runner's failure
policy: a member can exit while other tasks continue. The prepared route and port
remain owned by the live session, allowing that member to restart on the same port.
When the root command exits, Nook stops the remaining children and removes routes.

The tested Yarn foreach, direct Nx and Lerna continuous-task configurations keep
persistent siblings alive after a member error. Nook keeps the session and its
ports available for a restart. The gate verifies that the sibling still serves
HTTP, then ends the remaining tasks successfully and checks that the runner
reports the original failure and Nook cleans up. pnpm, Bun, Vite Task and the
moon system-task fixture stop their peers when the member fails.

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

### pnpm

Use recursive parallel execution for persistent development scripts:

```json
{"scripts":{"dev":"pnpm -r --parallel --filter '@myorg/*' run dev"}}
```

`--parallel` bypasses topological ordering and the normal concurrency limit; a
web package depending on a persistent API task must not wait for that task to
finish. Keep the root package outside the filter and leave
`includeWorkspaceRoot: false` in `pnpm-workspace.yaml` to avoid recursively
launching the root `dev` script. Native `--filter` selects packages; Nook does not
translate filters. The gate uses pnpm 12.15.0 with an offline local workspace
installation. Its `pmOnFail: ignore` setting keeps this isolated fixture from
downloading a package manager at runtime; existing projects should retain their
own version policy.

### Bun

A root script can use `bun run --parallel --filter '@myorg/*' dev`. Parallel mode
avoids waiting for a persistent dependency to finish. The gate checks both
`--shell=system` with the Node wrapper/server and `--bun --shell=bun` with the
wrapper and server actually running on Bun 1.4.4. Package scripts still resolve
`nook-run` from their installed dependencies. The wrapper preserves the command
boundary when Bun removes a leading `--` from its JavaScript arguments.

### Vite+ / Vite Task

`vp dev` starts Vite's built-in server. `vp run dev` executes the existing package
`dev` script (or a configured task); these commands are not interchangeable.
For example, keep each app's `dev` script as `nook-run -- vp dev`, then use a root
script such as:

```json
{"scripts":{"dev":"vp run --parallel --no-cache --filter '@myorg/*' dev"}}
```

Vite+ 1.4.0 accepts either a package filter or `-r`, not both together.
`--parallel` bypasses dependency ordering; use `--concurrency-limit` only with
enough slots for all selected persistent tasks. Scripts are uncached by default,
while tasks defined in `vite.config.ts` are cached by default. Keep persistent
tasks at `cache: false` and use `--no-cache` for the development invocation. The
gate checks both existing Node scripts through Vite Task and two real `vp dev`
servers. Those servers read `HOST`/`PORT` in their Vite configuration; this proof
does not require automatic framework flag injection for `vp`.

### Nx directly

Lerna coverage is separate from direct Nx coverage. The gate runs Nx 24.0.0
against package scripts with package-local working directories:

```sh
nx run-many -t dev --parallel=8 --skipNxCache --outputStyle=stream
nx run-many -t dev -p @myorg/api --skipNxCache --outputStyle=stream
```

Set `cache: false` and `continuous: true` on development targets. A dependency
such as `dependsOn: ["^dev"]` can then start an API and its web dependent
concurrently. Keep enough parallel slots and use `NX_DAEMON=false` for a
session-owned process tree. The fixture also disables Nx Cloud. Nook does not
replace the task graph or start a second Nx daemon.

### moon exploration

The bounded proof uses moon 2.6.2, explicit projects, and system-toolchain tasks
that execute each package's existing `npm run dev`:

```yaml
tasks:
  dev:
    command: npm run dev
    toolchains: system
    options:
      cache: false
      persistent: true
      outputStyle: stream
```

`moon run :dev` starts the two apps and watcher; `moon run api:dev` selects the
API. The fixture has a Git HEAD, disables automatic dependency installation,
and needs no managed toolchain download. This validates the inherited session
contract for system tasks, not moon's managed Node toolchain, affected runs,
remote caching, wait dependencies or daemon integration. The npm CLI release
2.6.3 referenced an unpublished Linux native package when checked, so the gate
pins the complete 2.6.2 release.

### Modern Yarn

The gate uses Yarn 4.18.1 with `nodeLinker: node-modules`, installed workspace
wrapper dependencies, and:

```sh
yarn workspaces foreach -A -p -i -j unlimited --include '@myorg/*' run dev
```

`-p` enables parallel tasks, `-i` streams output, and unlimited jobs avoids a
persistent task occupying the only available slot. Keep the root outside the
include pattern and avoid `--topological` for interdependent persistent scripts.
Use `--include @myorg/api` for a native package filter. Yarn foreach waits for
parallel siblings after a member error; Nook cleans up when the runner returns
or is stopped. This coverage makes no claim for Yarn Classic or Plug'n'Play.

Official references:

- [Turbo environment variables](https://turborepo.dev/docs/crafting-your-repository/using-environment-variables)
- [Turbo task configuration](https://turborepo.dev/docs/reference/configuration)
- [Lerna tasks](https://lerna.js.org/docs/features/run-tasks)
- [Lerna task pipelines](https://lerna.js.org/docs/concepts/task-pipeline-configuration)
- [pnpm run](https://pnpm.io/cli/run) and [settings](https://pnpm.io/settings)
- [Bun workspace filters](https://bun.com/docs/pm/filter) and [runtime](https://bun.com/docs/runtime)
- [Vite+ run](https://viteplus.dev/guide/run) and [task caching](https://viteplus.dev/guide/cache)
- [Nx tasks](https://nx.dev/docs/getting-started/tutorials/running-tasks) and [CLI](https://nx.dev/docs/reference/nx-commands)
- [moon tasks](https://moonrepo.dev/docs/config/project) and [workspace](https://moonrepo.dev/docs/config/workspace)
- [Yarn foreach](https://yarnpkg.com/cli/workspaces/foreach)

## Validation

The normal Rust test suite covers configuration, interpolation, native launches
and cleanup. The explicit runner gate uses real Turbo 2.11.10, Lerna 10.1.1,
pnpm 12.15.0, Bun 1.4.4, Vite+ 1.4.0, Nx 24.0.0, moon 2.6.2 and Yarn 4.18.1.
Each fixture has two HTTP apps using the repository's JavaScript wrapper and one
non-HTTP watcher. The Bun runtime and real Vite server variants share the same
contract assertions.

The gate verifies routes and leases before the single root command starts,
distinct app environments/cwds/domains/ports, session forwarding to wrappers and
removal from servers, native filtering, normal shutdown, standalone wrapper use,
native member-error
policies, and real-runner SIGKILL recovery through `prune`. The original shared
Turbo/Lerna regressions additionally cover member restarts with stable ports,
detached descendants, preservation of sibling apps, an unrelated protected
process, and recovery with a different temporary directory setting.

```sh
cargo build --locked
npm install --prefix target/workspace-runner-tools --no-audit --no-fund turbo@2.11.10 lerna@10.1.1 pnpm@12.15.0 bun@1.4.4 vite-plus@1.4.0 nx@24.0.0 @moonrepo/cli@2.6.2 @yarnpkg/cli-dist@4.18.1
NOOK_TEST_BUN="$PWD/target/workspace-runner-tools/node_modules/.bin/bun" node --test packages/nook-run/test/nook-run.test.js
node --test tests/workspace_runners.mjs
```

The gate requires Node 24.15+ and Caddy 2.11.x. On Linux it uses `unshare` and
`ip` to isolate port 80 in a private network namespace. Set
`NOOK_RUNNER_DIRECT=1` only when running with a free, permitted loopback HTTP
port 80. It uses temporary project, registry, package-manager and Caddy data
paths, makes no network installs during execution, and installs no CA.
`NOOK_TEST_RUNNERS=pnpm,bun,bun-runtime` selects focused fixtures; unknown names
fail. CI runs the full matrix with the pinned tool installations above.

Worktree-specific domain isolation is tracked separately in NOOK-74, after the
monorepo work. It is not inferred from branch names by this workspace format.
