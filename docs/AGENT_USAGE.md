# Use Nook with coding agents

Install the portable [Nook skill](../skills/nook/SKILL.md) in the **project that
uses Nook**. It teaches application launch, URL discovery, testing, diagnosis
and owned cleanup. It is not a contribution guide for the NTNook repository.
Caddy and its CA remain a one-time machine setup; normal usage is `nook run`.

## Install and discover

Copy the whole `skills/nook` directory from a reviewed NTNook checkout or source
archive containing this file. Pin that source to a commit or release; do not
silently refresh from `dev`. No release tag is claimed to contain this new skill
yet. Its runtime contract baseline is recorded in the skill metadata; installed
Nook capabilities must still be checked with `--version` and `run --help`.

| Harness | Project destination | Personal destination |
| --- | --- | --- |
| Codex | `.agents/skills/nook` | `~/.agents/skills/nook` |
| Claude Code | `.claude/skills/nook` | `~/.claude/skills/nook` |
| OpenCode | `.opencode/skills/nook` (also accepts `.agents/skills`) | `~/.config/opencode/skills/nook` |

Example from the consuming project, with a local reviewed checkout at
`../NTNook`. Choose one destination for each harness, avoiding duplicate copies
of the same skill within its discovery paths.

```sh
destination=.agents/skills/nook
if [ -e "$destination" ] || [ -L "$destination" ]; then
  echo "Skill already exists; review it before updating" >&2
else
  mkdir -p "$(dirname "$destination")"
  cp -R ../NTNook/skills/nook "$destination"
fi
```

```powershell
$destination = '.agents/skills/nook'
if (Test-Path -LiteralPath $destination) {
    throw 'Skill already exists; review it before updating'
}
New-Item -ItemType Directory -Force (Split-Path $destination) | Out-Null
Copy-Item -LiteralPath '../NTNook/skills/nook' -Destination $destination -Recurse
```

Copies work without symlink privileges on Windows. To update, compare the
installed folder against the selected source and replace only that installation
after reviewing local edits. To uninstall, remove only the skill you installed;
no Nook state or application configuration needs removing.

The optional external `skills` CLI can install from that local checkout:

```sh
npx skills add ../NTNook --list
npx skills add ../NTNook --skill nook --agent codex claude-code opencode --copy
```

It is not a Nook command or required dependency. Once a containing revision is
published, its documented `owner/repo@tag-or-sha` source syntax can pin a remote
installation. Do not advertise a containing release or marketplace listing
until verified. See the [installer's official reference](https://github.com/vercel-labs/skills/blob/main/docs/commands.md).

Verify that the harness lists `nook`, invoke it once, and confirm it can open a
bundled reference. Codex can use `$nook`; Claude Code can use `/nook`; other
harnesses use their own skill selector or natural-language discovery. Local
folders are not automatically available in cloud sessions. Check the official
[Codex](https://learn.chatgpt.com/docs/build-skills),
[Claude Code](https://code.claude.com/docs/en/skills), and
[OpenCode](https://opencode.ai/docs/skills/) discovery rules (checked 2026-11-11).

Without skill support, ask the agent to read `SKILL.md` explicitly. An optional
pointer in the consuming project's existing instructions can say:

> To launch, test, or diagnose this project's apps with Nook, read the installed
> Nook SKILL.md and only the references relevant to the task.

Supply the actual installed path; preserve the project's other instructions.
Installing the skill does not authorize changing scripts, Caddy, CA trust or
network exposure.

## Start small

Try: “Use Nook to start this app, verify its actual URL, run the existing E2E
command, then stop only the session you created.” The entrypoint routes to
configuration, workspaces/worktrees, verification or recovery on demand.

See [workspace runner details](WORKSPACES.md), [Docker setup](DOCKER.md), and
the [main setup guide](../README.md#prepare-caddy-for-nook) only when needed.
The [evaluation scenarios](../tests/agent_skills/scenarios.md) distinguish tested
behavior from future evaluation work. No provider-specific metadata, plugins,
hooks or MCP server are required by this skill.

## Capability boundary

The portable workflow uses CLI launch and lifecycle commands. This checkout also
provides optional [structured JSON/URL discovery and local MCP](MCP.md), with a
versioned output/error contract, read-only defaults and explicit alias grants.
Check the installed binary's help before using these additions; the skill's
baseline remains usable with older versions. MCP does not launch or stop apps.
Doctor, Compose orchestration and Cloudflare named tunnels remain separate
future milestones. Doctor must separate diagnosis from repair; Compose and
additional tunnels require separate ownership and exposure designs.

Existing LAN, standalone Tailscale Serve/Funnel and
[Cloudflare Quick Tunnels](CLOUDFLARE.md) and [ngrok](NGROK.md) are available.
ngrok uses an existing account and can select an already available domain; it
connects directly to the loopback app, bypassing Caddy middleware. Quick Tunnel URLs are
public and ephemeral; cloudflared installation is explicit. Serve is not supported for multi-app
workspaces; Funnel is excluded from worktree mode. The skill preserves those
limits and never turns Nook into a Caddy service manager.
