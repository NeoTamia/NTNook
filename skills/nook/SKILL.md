---
name: nook
description: Run, test, and diagnose project applications with Nook, including native workspaces, existing task runners, and concurrent Git worktrees. Use for Nook local URLs, readiness, logs, and owned cleanup; not for contributing to Nook itself.
metadata:
  compatibility: Linux or Windows x86-64, system Nook and an existing Caddy 2.11.x.
  contract-baseline: a2a4ce5e6e65145dfc880c626e2903a1211d38d3
---

# Use Nook in a project

Nook attaches owned routes to an existing Caddy. Caddy and its local CA are
prepared once; ordinary development then uses `nook run`.

## Choose the launch

1. Read the project's `nook.toml`, applicable `nook.local.toml`, package scripts,
   and existing test command. Identify the current directory and Git worktree.
2. Check `nook --version` and `nook run --help`. Use the installed command's
   capabilities, not a newer documentation example or the npm wrapper version. If Nook is missing, report
   the prerequisite; a wrapper's direct fallback does not provide Nook URLs.
3. Choose one supervisor:
   - **One app:** use its existing Nook script or `nook run`. Read
     [configuration](references/configuration.md) when choosing argv, ports,
     configuration files, or platform-specific settings.
   - **Native workspace:** use the version 2 manifest and `nook run`; Nook starts
     the declared commands. Read [workspaces](references/workspaces.md).
   - **Existing task runner:** keep its graph and scripts, and use
     `nook run --runner -- <root command>`. Read the runner section in
     [workspaces](references/workspaces.md) before changing package scripts.
   - **Concurrent worktrees:** enable `--worktree` or the existing manifest
     setting. Read the worktree section in [workspaces](references/workspaces.md).
4. Start that command once in a session you can track and stop. Record its
   session handle, working directory, actual hostname, and output.
5. Read the announced URL, then verify an application response before running
   tests. An announced URL is not readiness; Nook's `ready` state is TCP only.
   Read [verification](references/verification.md) for URL discovery, E2E and logs.
6. On failure, read [recovery](references/recovery.md). Diagnose the failing
   layer rather than changing the Caddy service or repeatedly relaunching apps.
7. If this was a temporary test session, stop it and verify cleanup. If the user
   asked to keep a server running, retain its session and report how to stop it.

## Ownership and completion

- Use Nook's lifecycle commands for its resources. Preserve foreign Caddy
  routes, unrelated processes, aliases, and other live worktrees.
- `list` and `status` can reconcile shared user state; they are not pure reads.
  `prune` is also user-wide, not scoped to the current directory.
- Use a full observed hostname for targeted stops. `run --force` transfers a
  Nook route without stopping its old process; it is not collision diagnosis.
- Keep Caddy service management, CA installation, DNS and firewall changes
  outside the ordinary launch. Nook never edits hosts files or elevates.
- Worktree isolation covers Nook names and process ownership, not application
  files, databases, or secrets. Do not claim those are isolated.
- Keep exposure local unless requested or already intentionally configured.
  LAN, Serve and Funnel have separate constraints in the recovery reference.

Finish with the actual URL, evidence of application readiness, the requested
test result, and whether your session remains running or was cleaned up.
Report pending cleanup explicitly; do not claim success from an exit code alone.

## Available interface

Use `run`, `init`, `list`, `status`, `stop`, `prune`, `alias`, `config`, and
`ca export` as documented by their installed `--help`. There is no `nook logs`,
`nook doctor`, `nook url`, `nook list --json`, MCP server, or Compose orchestration
in this contract. Do not substitute hypothetical commands for these workflows.
