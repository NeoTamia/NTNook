# Local MCP server

`nook mcp` exposes Nook to a local MCP client over stdio. Configure Caddy and its
CA once, then keep launching applications with `nook run` from a terminal or an
agent's shell. MCP does not start applications, stop processes, prune state,
start/reload/stop Caddy, install certificates, or change system configuration.

Linux and Windows x86-64 are supported. There is no network MCP listener,
macOS support, custom TLD, hosts-file change, or additional framework integration.

## Connect a client

Use a client that can launch an executable with arguments and communicate over
stdio. The enclosing configuration key depends on the client. For clients using
`mcpServers`, a Linux example is:

```json
{
  "mcpServers": {
    "nook": {
      "command": "/home/alice/.local/bin/nook",
      "args": ["mcp", "--cwd", "/home/alice/shop"]
    }
  }
}
```

On Windows, use the absolute executable path, for example
`C:\Users\Alice\AppData\Local\Programs\Nook\bin\nook.exe`, and pass
`["mcp", "--cwd", "C:\\src\\shop"]` as arguments. Do not wrap the executable in
a shell. Native Caddy's TCP Admin API is supported; Unix Admin sockets are Linux-only.

The official Rust SDK (`rmcp` 3.9.0) handles MCP `2026-07-28` discovery and
per-request metadata, plus `2025-11-25` and `2025-06-18` initialization. The wire
suite uses an independent client and tests all three versions on CI; this is not
an assertion that every GUI client's configuration format has been tested.

stdout contains only MCP messages; stderr is reserved for diagnostics. No update
check runs in MCP mode. Close stdin to shut the server down. Persistent aliases,
application processes and Caddy survive the MCP client's exit. No automatic
reconciliation occurs on server startup or shutdown.

## Profiles, configuration and ownership

```sh
nook mcp --cwd /home/alice/shop --config nook.toml --worktree=agent-a
nook mcp --scope user
nook mcp --tools read,aliases --allow-alias db.agent-a.shop.localhost
```

- `--tools read` is the default. `read,aliases` additionally enables alias tools
  and requires at least one `--allow-alias HOST`. Profiles cannot change at runtime.
- `--allow-alias` is repeatable and requires exact, complete `.localhost` hostnames;
  no wildcards. An allowed name is visible in the context scope too.
- `--scope context` is the default. With worktree mode enabled, runs are selected
  by their recorded worktree key, not by branch name or a guessed DNS suffix.
  Otherwise visibility follows the configured application hostnames. Matching
  names without worktree identity do not prove project ownership.
- Aliases have no project/worktree identity in the existing registry. Only explicit
  alias grants authorize mutation. A matching configured hostname grants visibility,
  not mutation rights.
- `--scope user` reads the user's whole registry but does not broaden alias grants.
- `--cwd` defaults to the launch directory and is canonicalized once. Relative
  `--config` paths resolve against it. There is no implicit upward manifest search;
  when launching inside a package, pass the workspace manifest explicitly.
- `--local`, `--worktree[=ID]`, `--no-worktree` retain `run` semantics. `--local`
  requires `--config`. The neighboring local file is otherwise ignored for an
  explicit configuration. No process-wide cwd is changed during requests.
- Configuration is captured at launch; restart the MCP server after editing it.
  Invalid project configuration is reported by status and does not prevent tool
  discovery. Malformed launch options or an invalid cwd exit with code 2.

MCP inherits the operating-system permissions of its user. Profiles are enforced
at dispatch and in mutation operations, but do not sandbox an agent that also has
shell access. Keep Caddy's Admin API private. Endpoints and state locations come
from the existing user configuration/environment and cannot be changed per call.
Do not share this server with an untrusted local client.

## Tools and JSON contract v1

Every tool has a closed input object (`additionalProperties: false`) and an
`outputSchema`. Unknown tools, disabled tools and invalid arguments return
protocol errors. No tool accepts arbitrary CLI arguments, environment variables,
commands, a new cwd, or a different Caddy endpoint.

| Tool | Inputs beyond optional `timeout_ms` | Behavior |
| --- | --- | --- |
| `nook_tools_profiles` | none; no timeout argument | Reports active/available profiles, scope, grants and tool names |
| `nook_list` | `kind`: `all`/`run`/`alias`/`compose` (default `all`); optional `hostname`, `limit` (1–200, default 100), `cursor` | Reads registered resources, including stale leases |
| `nook_url` | optional `hostname` or `app`; `mode`: `registered`/`configured` (default `registered`) | Looks up an observed resource or calculates a configured local URL |
| `nook_status` | `detail`: `summary`/`full` (default `summary`) | Inspects existing configuration, registry, Caddy and process identities; `full` also checks CA trust |
| `nook_alias_set` | required `hostname`, `target` (port 1–65535 or HTTP(S) URL); `tls` (default true), `preserve_host` (default false) | Creates or repairs an authorized persistent alias without replacing another owner |
| `nook_alias_remove` | required `hostname`, `expected_owner_id` (UUID) | Removes only the observed alias owner |

`timeout_ms` ranges from 100 to 30000, defaulting to 5000 for list/URL and 15000
for status/aliases. Four reads and one mutation may execute concurrently within
one server. Saturation returns `STATE_BUSY`. The existing interprocess operation
lock coordinates multiple MCP servers with the normal CLI. No caller can bypass it.

Example calls (the MCP host supplies the surrounding `tools/call` message):

```json
{"name":"nook_url","arguments":{"app":"web","mode":"configured"}}
{"name":"nook_alias_set","arguments":{"hostname":"db.localhost","target":3000}}
{"name":"nook_alias_remove","arguments":{"hostname":"db.localhost","expected_owner_id":"9e8e47a7-482b-4c7c-ad52-ddda81f8f91c"}}
```

For URL lookup, `hostname` and `app` are mutually exclusive; `configured` accepts
only an app selector. Without a selector a single app is required. Workspace
ambiguity returns `AMBIGUOUS_APP`; an absent registration returns `NOT_FOUND`.
A configured URL does not reserve a route or prove availability. No command is
required to resolve an application's name. URL schemes describe the frontend TLS,
independently of the upstream scheme. Tailscale allocation is never predicted.

List entries contain `resource_id`, `kind`, `hostname`, `url`, `target`, `tls`,
`preserve_host`, recorded `state`, observed `liveness`, `pid`, `group_id`, optional
`worktree` identity, and known `urls` (`local`, `lan`, `tailscale`, `funnel`).
Unavailable values are null. Recorded readiness and process liveness are distinct;
neither promises a successful HTTP response. Sensitive argv/environment and raw
Caddy configuration are not returned. Cursors are tied to the observed entries and
filters; `STALE_CURSOR` means restart pagination.

Success results use:

```json
{
  "schema_version": 1,
  "ok": true,
  "observed_at_unix_ms": 1794355200000,
  "context": {"scope":"context","worktree_key":null,"worktree_label":null},
  "data": {"entries":[],"next_cursor":null},
  "warnings": []
}
```

A failure replaces `data` with `error`:

```json
{
  "code":"OWNERSHIP_CHANGED",
  "message":"The hostname no longer belongs to the expected alias",
  "retryable":false,
  "operation_id":null,
  "effect":"none",
  "remediation":[]
}
```

The envelope is available as `structuredContent` and as the same JSON in a text
content block. `isError` follows `!ok`. Parse codes, not human messages. Error
codes are `CONFIG_INVALID`, `STATE_INVALID`, `STATE_VERSION_UNSUPPORTED`,
`STATE_BUSY`, `NOT_FOUND`, `AMBIGUOUS_APP`, `OUT_OF_SCOPE`, `CONFLICT`,
`FOREIGN_ROUTE`, `OWNERSHIP_CHANGED`, `UNSUPPORTED_EXPOSURE`, `CADDY_UNAVAILABLE`,
`CADDY_CONFLICT`, `PERMISSION_DENIED`, `STALE_CURSOR`, `TIMEOUT`,
`RECOVERY_PENDING`, and `INTERNAL`. `effect` is `none`, `applied`, or `unknown`.

The JSON contract version is independent of MCP protocol versions, Nook's release
version and registry format 2. Consumers should tolerate additive output fields.
Breaking changes to field types or meaning require a new contract version.

## Diagnostics and recovery

Status returns `health` (`ok`, `degraded`, `error`, `unknown`) and `checks` with
stable IDs: `config`, `registry`, `caddy_admin`, `servers`, `managed_routes`,
`pending_operations`, `process_liveness`, and, in full mode, `local_ca`. Each check
has its context/infrastructure scope, status, code, message, remediation and affected
resource IDs. An unavailable dependency yields unknown dependent checks. Untrusted
CA is never silently considered trusted. Remediations are text, not executed commands.
A diagnostic report describing broken infrastructure is a successful tool result.

Identical alias sets retain the existing UUID and do nothing when the route matches.
Different settings, live leases, or foreign routes conflict. There is no `force`.
Alias removal requires the previously observed owner UUID; a replacement owner is
preserved. Alias operations with a tracked Serve/Funnel registration are refused.
Use the existing CLI to make that broader change explicitly. Alias targets retain
Nook's existing restrictions and upstream TLS verification.

Writes are journaled before changing Caddy, use the existing ETag retries, and
modify owned route containers only. An interruption after a remote write may have
an unknown effect. Inspect status before retrying. Retrying the same authorized
alias operation can finish its pending work; `nook prune` remains the explicit
user-wide recovery command. MCP never reconciles unrelated resources automatically.
A client crash does not imply rollback or deletion of persistent aliases.

Cancellation stops further steps cooperatively; an already-issued network request
can finish within its remaining deadline. Blocking workers retain their concurrency
permit until they finish. EOF cancels handlers through the SDK, followed by bounded
transport/runtime draining (up to ten seconds). Journaled operations survive forced
termination. Git discovery is noninteractive and bounded to five seconds per command.
MCP messages are limited to 1 MiB input and 4 MiB output. The official SDK discards
unparsable/oversized lines and keeps the connection usable; valid JSON with an invalid
JSON-RPC shape receives a protocol error. It does not echo malformed input.

## Shell and agent skills

```sh
nook list --read-only --json
nook alias list --read-only --json
nook status --read-only --json
nook url --app web --config nook.toml --worktree=agent-a --mode configured --json
```

`--json` for list/status/alias list requires `--read-only`. Existing commands without
these options retain their reconciliation behavior. Read-only list CLI results are
paginated like MCP (first 100 entries); use MCP's cursor for additional pages.
`nook url` is always read-only and prints only its URL without `--json`.
JSON exit codes are 0 for success, 1 for a domain failure or status health `error`,
and 2 for invalid CLI arguments. Status's read-only CLI uses full diagnostics.

The [portable agent skill](AGENT_USAGE.md) can use these optional interfaces after
checking installed capabilities. An agent should inspect status, resolve the URL,
launch `nook run` in its shell if needed, then observe the registered resource.
The CLI `nook doctor` also observes Compose ownership without calling Docker. V1 has no
`nook_doctor`, process launch/stop tool, or MCP prune.

## Validation

`cargo test --test mcp_protocol -- --test-threads=1` exercises independent JSON-RPC
wire calls, generated output schemas, three protocol versions, CLI JSON, commandless
workspace URL lookup, worktree identity filtering, pagination, cancellation and a
real Caddy alias lifecycle. Linux runs the real-Caddy case in an isolated network
namespace unless `NOOK_E2E_DIRECT=1` and covers HTTP and HTTPS. Windows uses native
`caddy.exe` on HTTPS port 443 because hosted runners reserve HTTP port 80. HTTPS
requests use the fixture CA via curl `--cacert`, without installing it. Windows
also uses `--ssl-revoke-best-effort` because the ephemeral CA has no CRL
distribution point; certificate chain and hostname verification remain enabled.
Tests create only temporary configuration/state and never install a CA.

Compose entries include a `compose` object with `project`, `service`, `port`,
`active`, `desired`, and `last_status` from the registry. Their liveness is always
`unknown`: read tools do not inspect Docker or update saved publications. Context
scope includes Compose projects whose canonical root equals `--cwd`; an explicit
hostname grant or `--scope user` can expose other projects. A matching configured
hostname alone does not expose another Compose worktree. Inactive reservations
remain visible but do not require a Caddy route; a leftover route is reported as
drift. Alias mutation tools cannot take over active or inactive Compose owners.
