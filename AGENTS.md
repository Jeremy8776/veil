# Veil session workflow

## Mission

Veil is a **Modular, Agnostic, Shared video/audio calling app and embeddable call provider**.
Local first, peer to peer by default, usable on a surviving network when internet fails.
People own their identities and relationships. Chosen helpers are replaceable and receive
no conversation keys. Easier connection never bypasses identity checks, admission, removal,
privacy policy or resource limits. Other apps should consume the shared engine/provider,
not copy a private app engine or create a forked protocol.

## At the start of every chat/session

Before substantive work:

1. Read `docs/ROADMAP.md`, `docs/roadmap-state.json` if present, and the current snapshot of
   `docs/HANDOFF.md`. Inspect the code/branch relevant to the task; do not treat old notes as
   proof that code has shipped or a test has run.
2. From this repository, run:
   `python tools/roadmap.py start --session <chat-id-or-unique-session-id> --title "<short task title>"`
   Reuse the same session ID in this chat. This starts/reuses a loopback-only live preview
   and registers the session. If Markdown is missing, install the exact dependency in
   `tools/requirements-roadmap.txt` using the normal permitted environment workflow.
3. Open the returned URL at `#live` in the user's Codex browser panel with
   `mcp__codex_app__open_in_codex` (browser target). Open once at session start; do not repeatedly
   steal focus on every turn. If no preview tool is available, provide the URL honestly.
4. State how the requested work relates to the highlighted next step. Honour explicit user
   scope; a roadmap priority is not permission to implement unrelated features.

For a chat already in progress, re-read this file and current state before continuing if
the user asks to resume or project priorities have changed. Reopen the live roadmap when
it is not already available. Do not create an automation or a new chat for this workflow.

## Keep the roadmap live while working

Update the shared state at meaningful transitions, not only in the final reply:

```text
python tools/roadmap.py update SDK-01 --status in_progress --note "Designing the provider lifecycle contract" --session <id>
python tools/roadmap.py update SDK-01 --status blocked --note "Concrete missing prerequisite and next action" --session <id>
python tools/roadmap.py update SDK-01 --status done --note "Full acceptance gate met" --evidence "Path/commit and actual verification result" --session <id>
```

Use the capability actually being worked on. Work states describe full capability gates;
existing partial implementation remains described in the roadmap. A small subtask does not
complete the whole capability. Never fabricate progress from elapsed time, a build alone,
the existence of a PR or a successful synthetic test. Keep user-reported and automated
evidence distinct. Keep secrets, reusable invites and conversation content out of state/logs.

Use `python tools/roadmap.py session --session <id> --status active --note "<current action>"`
to report work that is not itself a product capability, including documentation and tooling.
Do not mark AND-02 in progress merely because the roadmap UI was changed.

When priorities genuinely change, set a concrete next action:

```text
python tools/roadmap.py next SDK-01 --title "Define the headless provider lifecycle" --why "Reason tied to the mission" --check "Reviewable completion condition" --session <id>
```

Completing or blocking the selected capability advances the highlight to the next eligible
core capability in the recorded order. Verify that action still matches the user's direction.
The CLI serialises writers and preserves other sessions' changes. Do not rewrite the entire
JSON state manually or mark another session ended without evidence.

Update `docs/ROADMAP.md` when scope, dependencies or acceptance criteria change, and refresh
handoff/changelog when appropriate. `tools/render-roadmap.py` generates `docs/roadmap.html`;
edit the source/renderer rather than only the generated HTML. The live page polls recorded
state and reloads document revisions; it is not an automatic observer of every tool call or
all coding activity. Keep the local preview read-only, loopback-only and free of telemetry.

## Before yielding or finishing

Record actual evidence and blockers, keep the next step useful, and run:

`python tools/roadmap.py session --session <id> --status idle --note "<what changed; what remains next>"`

Use `ended` only when that session is deliberately closed. Do not leave the page claiming
active work after yielding. The preview may remain running locally so the user can inspect it.
Use `python tools/roadmap.py status` to inspect state and `python tools/roadmap.py render`
to regenerate a saved offline snapshot. Verify relevant changes before claiming completion.
