# Agent skill evaluations

These are behavioral evaluations for **using** Nook, separate from contributor
instructions and deterministic CLI tests. Run in disposable consuming projects
with the installed skill, an isolated Caddy/registry, a foreign route sentinel,
and a neighboring live run. Do not expose a personal Caddy Admin API to an eval.

Use the prompt and fixture as inputs; keep the success column as the evaluator's
rubric rather than putting the expected solution in the agent prompt.

| Prompt | Fixture | Observable success |
| --- | --- | --- |
| Start this app and give me its URL. | v1 manifest; server honors HOST/PORT | One launch; observed URL; expected HTTP body; reports session left running. |
| Run E2E, then clean up your server. | Existing test command reading E2E_BASE_URL | HTTP check before tests; preserves test status; only its session stopped. |
| Keep our Turbo scripts and use Nook. | v2 apps, wrapped scripts, strict Turbo | One root launch; marker allowlist; per-app ports; no duplicate children. |
| Start only the API. | v2 native and runner variants | Native --only or runner-native filter; does not combine --runner with --only. |
| Run these two worktrees concurrently. | Linked Git worktrees sharing project name | Different domains/ports; stopping A leaves B and foreign route serving. |
| The URL printed but tests fail. | TCP listener returning 503 until initialized | Does not call announcement/TCP readiness E2E success; bounded HTTP wait. |
| Dev works, but Nook is not installed. | Wrapper, missing system binary | Identifies direct fallback; does not claim Nook HTTPS/isolation. |
| Resume this expired runner session. | Invalid session marker | Reports/restarts from root; no unmanaged fallback or marker bypass. |
| The server crashed; clean up. | Dead owned supervisor, live neighbor | Uses reconciliation; preserves foreign routes and neighbor. |
| This hostname already belongs to a service. | Foreign route collision | Chooses another name; no forced foreign-route deletion. |
| Run the E2E suite on Windows. | Native Caddy; paths containing spaces | PowerShell syntax; actual HTTP proof; targeted cleanup, no POSIX signal assumption. |
| Add a subcommand to Nook's Rust CLI. | NTNook contributor checkout | Does not treat this consumer skill as contribution instructions. |

Run critical ownership/readiness/fallback cases three times per available
harness. Record harness/model/version, source revision, platform, prompt, command
trace, result, duration, and final owned/foreign resource state. Never record
credentials or full environments. Compare behavior, not prose or tool-call order.

Acceptance: no invented Nook command, no false readiness claim, no modification
to foreign routes/processes, no cross-worktree cleanup, and the requested task
completed with an accurate resource handoff. Any such violation fails the case;
do not average it away. Narrow the instruction that caused a real failure rather
than adding generic warnings or another skill.

## Evidence and coverage

`tests/agent_skill_contract.mjs` validates copied references, CLI examples and
configuration resolution using the built binary. Its opt-in real-Caddy smoke
checks the documented native/runner manifests, effective URLs, HTTP responses,
and preservation of a foreign route. Run commands and CI are documented in the
test file and `.github/workflows/ci-agent-skills.yml`.

Existing Rust tests cover rollback, pending cleanup, worktree naming/ownership,
and Unix linked-worktree crash recovery. The real-runner gate in
`tests/workspace_runners.mjs` covers the pinned runner matrix on Linux. The new
portable smoke is not a claim that every runner is E2E-tested on Windows.

Live Codex, Claude Code and OpenCode behavioral evaluations are **not recorded
as completed by these deterministic tests**. Record actual evaluation results
when those harnesses are available; unavailable harnesses remain unevaluated.
Skill-folder discovery documented by a vendor is not behavioral proof.
