# Live roadmap tracking

Veil sessions start by opening the roadmap. `AGENTS.md` in this repository contains the agent
startup/update protocol; the parent workspace instructions also direct sessions here. These
instructions apply when a session loads this workspace, not through a global operating-system
hook. Existing chats should reload the instructions before resuming Veil work.

## Start and view

From the repository:

```text
python tools/roadmap.py start --session my-session --title "Short task title"
```

Open the returned local URL in Codex. The command reuses this repository's running service
or starts a hidden local process, normally at `http://127.0.0.1:8766/#live`. If that port is
unavailable it selects another port and records it in the ignored `.roadmap-runtime.json`.
The service listens only on loopback, serves a documentation allow-list and read-only state,
and exposes no browser mutation endpoint. It is not a public website or a telemetry service.

The renderer needs `Markdown` from `tools/requirements-roadmap.txt`. The state CLI/server use
Python's standard library. Regenerate an offline snapshot with:

```text
python tools/roadmap.py render
```

## What live means

The live page reads shared `docs/roadmap-state.json` every two seconds while visible. Updates
to recorded work states, notes, evidence and next step appear without manual refresh. Changes
to the roadmap document or renderer regenerate/reload the page, preserving open capabilities,
filters and scroll position when browser storage is available. It reports sync/disconnection
explicitly. The file-based HTML displays saved state and a link to the live preview.

The state is updated deliberately by working sessions. It does not infer progress from every
tool call, time spent, changed files or a PR being opened. Session activity is labelled with
its last reported timestamp; an interrupted agent can leave a stale report. Agents must mark
their session idle before yielding and refresh their report when resuming.

## Work states and next action

Work states are `planned`, `in_progress`, `blocked` and `done`. They describe full capability
acceptance gates, not whether some underlying code already exists. Each capability's existing
implementation/protection state is still explained in the roadmap. No percentage is inferred.

```text
python tools/roadmap.py update AND-02 --status in_progress --note "Reviewing the existing Android extraction" --session my-session
python tools/roadmap.py update AND-02 --status blocked --note "Concrete missing prerequisite" --session my-session
python tools/roadmap.py update AND-02 --status done --note "Full gate verified" --evidence "Review/build/CI record" --session my-session
```

Marking a gate complete requires nonempty evidence. Completing or blocking the highlighted
capability selects the next eligible core capability in the recorded priority order; it does
not start that work or mark it active. Dependencies must have their gates closed before
automatic selection. Explicitly change priority with a useful action and completion checks:

```text
python tools/roadmap.py next SDK-01 --title "Define the provider lifecycle" --why "Enable reusable calling without app coupling" --check "Review the command/event and teardown contract" --session my-session
```

Multiple writers use a file lock and atomic replacement, preserving concurrent updates. The
activity list retains the latest 100 recorded transitions. State JSON is a repository artifact;
runtime files, lock and server log are ignored. Conflicting Git edits still need normal review.

## Session lifecycle

```text
python tools/roadmap.py session --session my-session --status active --note "Current meaningful action"
python tools/roadmap.py session --session my-session --status idle --note "What changed and what remains next"
python tools/roadmap.py status
```

Use `ended` for a deliberately closed session. Do not mark another session ended merely
because it has not updated recently. Keep secrets, reusable invites and conversation content
out of notes/evidence. A browser heartbeat means the preview is connected, not that a coding
agent is currently working or a capability is complete.
