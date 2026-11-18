# Connection paths start in stages

Status: accepted (2026-11-18)

Veil can reach a peer directly, by a punched dial through the rendezvous, or through a relay. It
used to try them one after another, so a person behind carrier-grade NAT could wait about half a
minute while each path timed out. The paths now overlap (`veil-call/src/dial.rs`). The direct dial
starts at once, the punched dial after 1 second and the relay after 5 seconds. A path that fails
starts the next one at once, and the first link up is used.

## Considered options

- **One path after another** (the old behaviour). Rejected because of the wait.
- **Start every path at once.** Rejected. Each call would ask the rendezvous for an introduction
  even when a direct dial was about to succeed.
- **Connect through the relay at once, then move media to a direct path** (branch
  `claude/relay-first-166de4`). Rejected for now. It connects fastest, but every link opens a relay
  session, so the relay sees every pair of members and its limited places fill faster. It also
  predates the fixes that protect those places.
- **Stages** (chosen). A call that connects directly within 1 second uses neither the rendezvous's
  introduction nor the relay. One that connects within 5 seconds opens no relay session.

## Consequences

- A link stays on the path it started on. A slow direct path that would have worked after 5 seconds
  loses to the relay for that link. If relayed links prove common, move media to a better path
  during the call.
- A dial still running after 1 second may trigger an introduction even if the direct path then
  wins. The existing dial-back limits apply.
- A relay-only node still uses only the relay, so peers never learn its address.
- Links from losing paths never open a control stream, so the peer routes only the winner.
- The 1 and 5 second values are reasoned, not measured on real mobile networks. Change them if
  field results say otherwise.
