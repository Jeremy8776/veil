# Local connection reports

Open **Connection report** on Home, after a call ends, or from the call toolbar (desktop)
or **More** menu (Android). Desktop also supports **Ctrl+Shift+E**. In Connection settings,
run **Test these settings**, then **Preview connection report**. A check uses a temporary
identity; it does not start a call, register a room or change hosting permission.

Opening a report reads recorded local evidence. It does not test the network or upload
anything. The last call and last completed check remain separately available for this app
run. They are not a persistent history. Editing settings invalidates the check's active
result; an already captured report remains a historical snapshot of its recorded policy.

## Reading the evidence

- **Helper connection** confirms a pinned connection, including one which returned a refusal.
  It does not prove registration, hosting permission or admission.
- **Permission to host** is confirmed by successful host registration. An authenticated
  `NotAllowed` refusal says this device needs an operator grant; guests still need admission.
- **Invitation**, **Host connection**, **Host admission** and **Call membership** describe
  separate stages. Not observed means no confirming event was recorded.
- Recovery is explicit; host ending, removal and recovery failure are different outcomes.
  Removal stops recovery evidence and does not offer automatic readmission.
- Each admitted peer has its current media route and recent decoded audio/video observations
  (three-second window). A pending link, membership keys or transport statistics alone do
  not establish usable media. Silence or disabled capture may be intentional. This device
  cannot prove another device receives or plays its media, or that received sound is audible.
- Capture is off, waiting for frames, frames observed, unavailable, or permission not granted.
  Android uses its permission state. Desktop driver failures report unavailable and suggest
  checking device choice and system permissions; they do not guess an OS denial from text.
- **No verified UDP reply** covers a silent endpoint. Filtering, NAT and an unavailable host
  can look alike. This evidence cannot establish the cause; Veil has no TCP fallback yet.
  **No permitted route** remains separate from an identity-verification failure.

The Local only / Hide my address policy shown belongs to the recorded call or check, not
necessarily the latest settings draft. Diagnostics do not relax that policy or key checks.

## Choosing what to share

The preview is frozen when opened. **Capture latest snapshot** explicitly refreshes it.
Live engine updates cannot alter the text between review and **Copy previewed report**.
Both sensitive-detail choices start off whenever the report is opened or its source changes:

- **Include candidate IP addresses and ports** includes bounded literal endpoints associated
  with the attempt. A candidate may not have been dialled or verified. Hostnames/settings
  strings are not exported.
- **Include device public keys** includes the local, participant and endpoint public keys
  recorded by the source. A temporary check has no saved local device identity.

The exact JSON is visible before copying. Copying is a local clipboard action, not an upload.
Anyone you later send it to can see the selected metadata and the remaining call-state,
platform, version, participant-count, route, capture and statistics information in the preview.

The `veil-diagnostics-v1` export constructs an explicit field allow-list from typed evidence.
It accepts no names, raw error strings, raw log lines, chat, reactions, invitations, private
identity keys, MLS state or frame keys. Arrays are bounded to 128 peers and 16 candidate
endpoints; maximum-detail regression coverage checks output below 64 KiB. No automatic
export or upload service is introduced. Existing local log rotation remains at 5 MiB per
file, one current and one previous; raw logs are a separate surface and are not this export.

## Engineering verification

Checkout: `main/target/diag-01`, branch `codex/diag-01`, based on private TRUST-02 commit
`2ee20cd83c4d39421d65358d377c4812ab05eb6b`. The older `source/` checkout is unchanged.

Regression coverage includes a real wrong-key helper handshake, authenticated host refusal,
a silent bound UDP endpoint, no-address/no-route classification, permission-state mapping,
missing versus recent decoded media, capture freshness, recovery/removal, metadata opt-ins,
untrusted error-text exclusion, bounded exports, frozen previews, exact copied JSON, and
264/320/540-point report layouts. Tests are offline except for isolated loopback traffic.

Windows native UI inspection uses `dev-ui`, `VEIL_START=report`, isolated workspace app data,
Local only, and synthetic evidence. The continuation rebuilt `9256b92` and confirmed the
bounded window, scrolling to export controls, local copy acknowledgement, parsed clipboard
JSON matching the displayed preview, independent address/key choices, both choices reset on
reopening, and explicit snapshot refresh. The earlier Escape-interrupted check remains
historical; this is a separate completed inspection. The synthetic report's recorded policy
is independent of the current Local only settings and is labelled as synthetic evidence.
The preview starts no call or capture devices.

Android acceptance then passed on Samsung SM-S9260 / Android 17 after the user connected it.
The same-signer ARM64 APK retained the identity and settings. Camera and microphone requests
were explicitly denied; the real call report showed both permissions denied with useful
guidance, separately from confirmed membership and a direct route to the admitted desktop
test peer. No capture service was active. A brief pause of only the test peer expired the
three-second media window: the report showed no recent decoded audio/video and the specific
missing-media guidance. The frozen snapshot remained unchanged after the peer resumed;
Capture latest snapshot then restored the recent-audio observation. The test peer sends
sealed synthetic probes, not conversational media; this is not listening-quality evidence.
Android export controls, local copy acknowledgement, independent metadata choices and both
choices resetting on reopening were inspected. Leave ended the call for both devices.
Original connection/preferences bytes and identity SHA-256 matched after restoration; both
capture permissions and their original user flags were restored, and the app was stopped.

This check found and fixed Android's Show QR open/render lifecycle in `c565715`: the QR now
opens, persists and decodes locally. Affected app regressions passed 294 tests (11 ignored);
desktop app/all-target/dev-ui and Android ARM64 library Clippy passed with warnings denied.
The earlier 858-test workspace evidence remains valid for the diagnostic implementation.
Evidence, APK/build hashes, screenshots and restoration checks are indexed by
`main/target/diag-01-evidence/acceptance-final.json`. DIAG-01's acceptance gate is met on this
branch; the roadmap marks it done when PR #88 merges.
Existing multi-device physical outage and sustained mobile/media quality gates remain
separate roadmap work. No release, remote CI, push or publication is implied.

Reproduce from this checkout with its own target directory:

```text
cargo test --workspace --features veil-app/dev-ui --locked --offline -j 8
cargo clippy --workspace --all-targets --features veil-app/dev-ui --locked --offline -j 8 -- -D warnings
python tools/licences.py
git diff --check
```
