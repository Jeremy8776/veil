# A persistent call survives its host leaving, through a signed planned handover

Status: proposed (2026-11-27). Design for HOST-01, first pass. Not implemented.

Today the host is the only MLS committer (`crates/veil-group/src/group.rs`, leaf 0). Members
refuse any other commit, so the group cannot change without the host and the call ends when
the host leaves (`Control::Ended`).

This ADR adds a **Persistent call** switch. When the host of a persistent call leaves on
purpose, Veil shows who can take over. The leaving host can pick one person. If they do not
pick, or leave without choosing, Veil picks the available member with the best connection.
Authority moves inside MLS itself, in one commit, so every member agrees on who the host is
at every epoch.

Scope is **planned handover only**: the host leaves through Veil. A crash, a power cut or a
network partition is unplanned host loss. It keeps today's behaviour (20-second control
recovery, then the call ends) and needs a separate election design (section 10).

## 1. Rules that do not change

- The old host's private identity key never leaves its device. The new host hosts with its
  own device key. Members see a new host key, and safety codes stay per device.
- A removed participant is never restored. The roster after handover is exactly the MLS
  roster at the handover epoch, minus the old host.
- Easier continuity never bypasses admission. New joiners still go through the new host's
  lobby with a new invite.
- A relay-only member stays relay-only. No handover step sends any member's address to the
  new host or to other members.
- Being host is opt-in. Nobody becomes host without having said they are available.

## 2. Who may commit after transfer

There is still exactly one committer per epoch. What changes is how members find it: by a
group context extension, not by leaf 0.

- A persistent call's group carries a Veil extension in its MLS group context:
  `CallAuthority { host: PublicId, persistent: bool }`. The group lists the extension in its
  required capabilities, so a device whose key package does not support it cannot be added.
  An older Veil cannot join a persistent call; it is refused with "Update Veil to join this
  call".
- A member accepts a commit only if its sender leaf belongs to `CallAuthority.host` for the
  epoch the commit applies to. `Group::is_host()` becomes "my device is `CallAuthority.host`".
  For a call that is not persistent, `host` stays the creator and behaviour is unchanged.
- **Handover commit.** The current host commits one GroupContextExtensions proposal that sets
  `host` to the successor. This moves the group from epoch E to E+1. Members check that the
  successor is a current member and that the commit came from the current host.
- MLS does not let a committer remove itself in its own commit. So the new host's first act
  at E+1 is to commit the removal of the old host, moving to E+2. From E+2 the old host
  cannot decrypt new media.

`group.rs` is near the soft size limit. The authority check and extension codec go in a new
`crates/veil-group/src/authority.rs` with its own tests.

## 3. How the old host is fenced

The fence is the epoch. Once a member merges E+1, the old host is an ordinary member.

- Members refuse commits, `Plan`, `ProbePlan`, roster tickets, `Moderators`, `Waiting` and
  `Ended` that are signed by, or sent from, a key that is not `CallAuthority.host` for the
  member's current epoch. A stale `Ended` from the old host cannot end the call.
- Signed plans and roster tickets already name their epoch and signer. Members now also check
  the signer against `CallAuthority.host` at that epoch, not against the invite's host key.
- The old host marks itself not-host as soon as it merges its own handover commit. It answers
  no `Hello`, `KeyPackage` or `Resume` after that.
- Forwarder rooms are keyed by (room tag, host key), so the old host's tickets cannot open the
  new host's forwarder room.
- The old host deletes its group state when it leaves. The new host's E+2 removal makes any
  copy it kept useless for new media.

## 4. Conflicting coordinators

In a planned handover only the current host can commit at epoch E. Two hosts can only appear
if the current host signs two different handover commits for E (a faulty or malicious host).

- MLS lets each member merge one commit per epoch, so each member follows the first valid one
  it receives. The old host sends the handover commit over the same ordered control links it
  uses for every commit, so honest members see one commit.
- If members split anyway, their epoch secrets differ. They cannot decrypt each other's media,
  and each side's host sees the other side fail to `Resume`. Veil does not try to merge the
  sides. A member that cannot resume with its new host within the recovery window is told the
  call was lost (section 8).
- The successor is a single device. Veil does not name backup successors in the commit,
  because two backups that both think the first one failed would both commit E+2. Ordered
  fallback belongs with unplanned election (section 10).

## 5. Who is available, and what consent the new host gives

Hosting exposes the host's addresses in every invite, runs a rendezvous, relay and forwarder
on extra ports, and may open a port on the router. So availability is a choice, made before it
is needed and with that explained.

- In a persistent call each member sees a switch, **Available to host**, off by default. Its
  tooltip says: "If the host leaves, you may take over. Your address goes into new invite
  links, and Veil may open a port on your router if you allow that in Settings."
- A member cannot be available while **Hide my address** is on. The switch is disabled with
  the tooltip "Turn off Hide my address to host". The planner never picks a relay-only member.
- A device that cannot run host roles (for example a phone whose platform cannot keep a
  listener open) reports that it is not available.
- Turning the switch on is the consent. The handover itself does not ask again, because the
  leaving host should not wait on someone who stepped away. The new host sees a notice: "You
  are now the host. Earlier invite links no longer work." It can hand over again or end the
  call like any host.

## 6. What "best connection" means and how it is measured

Each available member sends a small `Availability` report to the current host only, over its
existing authenticated control link, when the switch changes and at most every 10 seconds:

| Field | Meaning | Source |
|---|---|---|
| `available` | The switch is on and the device can host | The member's own settings |
| `reachable` | Directly reachable without a relay: Good, Maybe (needs a punch), or No | Its own reflected and mapped addresses, reduced to one of three values |
| `direct_links` | Number of call members it has a direct (not relayed) media link to | Its own route table |
| `rtt_ms` | Median round trip to those members, in 10 ms steps, capped at 1 s | Existing `Ping`/`Pong` and media stats |
| `upload_kbps` | Recent achieved upload, in 500 kbit/s steps | Existing send stats |

No field carries an address, a member list or per-peer figures. The host already holds each
member's own reported addresses; the report adds only coarse, derived numbers.

The automatic pick sorts available members by, in order: `reachable` (Good before Maybe; No
is never picked automatically), then more `direct_links`, then lower `rtt_ms`, then higher
`upload_kbps`, then lower device key bytes as a fixed tie-break. The rule is a pure function
in `crates/veil-call/src/host/successor.rs`, tested on its own.

The leaving host's list shows each available person's name with a three-step bar (Good, OK,
Poor) from the same sort. It shows no numbers and no addresses.

## 7. The handover sequence

1. The host of a persistent call chooses Leave. A dialog shows: "Hand over hosting" with
   **Automatic (best connection)** selected, then the available members with their bars, and
   the buttons **Hand over and leave**, **End call for everyone** and **Cancel**. Closing the
   window or quitting Veil during a persistent call counts as Automatic.
2. The host sends `HandoverOffer { request, epoch }` to the chosen successor.
3. The successor starts its host roles with its own settings: listener, its configured
   `veil-node` or host-run rendezvous, relay and forwarder. It replies
   `HandoverReady { request, contact }`, where `contact` is the same signed address set an
   invite would carry, signed by the successor's own device key. If it cannot start within
   10 seconds, or declines, it replies `HandoverReady` with `ok: false`.
4. On a failure the host tries the next member in the automatic order, at most three in all.
5. The host commits the handover (section 2) and sends `Commit` and then
   `HandedOver { epoch, contact, removed }` to every member. `removed` lists, at most 64,
   device keys the old host removed or declined in this call.
6. Each member merges E+1, checks `contact` is signed by `CallAuthority.host`, and opens a
   control link to the new host with the existing `Resume { room_id, epoch }`. A relay-only
   member dials only through a relay (section 9). Media links between members stay up, so
   sound and pictures continue on mesh paths during the change.
7. The new host treats every member of its E+1 roster as a recovering device, with the
   existing 20-second window and commit journal (`host/recovery.rs`). It commits removal of
   the old host (E+2), then of anyone who did not resume in time, and sends a new plan and new
   roster tickets.
8. The old host stays connected until it sees the E+2 commit or 10 seconds pass, then closes.
   While waiting it shows "Handing over to Ana…". Quitting Veil waits for this, at most 15
   seconds in all.

New messages are `HandoverOffer`, `HandoverReady`, `HandedOver` and `Availability`. Each is
bounded by the existing control frame limit. `Ended` keeps its meaning for a call that ends.

## 8. What users see when continuity is impossible

| Situation | Who | What they see |
|---|---|---|
| The call is not persistent | Host | Leave ends the call, as today |
| Nobody is available | Leaving host | "No one can take over. Leaving ends the call for everyone." Only **End call** and **Cancel** |
| Three successors fail to get ready | Leaving host | "Could not hand over. End the call for everyone, or stay?" |
| The successor fails after the commit | Everyone | "The call was lost while the host changed. Ask for a new link." Old host cannot take authority back |
| A member cannot reach the new host in 20 s | That member | The same "call was lost" message |
| A relay-only member declines the new relay | That member | "You left the call." (section 9) |
| A joiner was waiting in the old host's lobby | That joiner | Refused with a new reason, `HostChanged`: "The host changed. Ask for a new link." |
| Unplanned host loss (crash, network) | Everyone | Today's behaviour: recovery for 20 s, then "The host left. The call has ended." |

Every member sees one line in the call when the host changes: "Ana is now the host."

## 9. Rendezvous room, invites, relay and forwarder

Nothing that belongs to the old host moves. The new host brings its own.

- **Rendezvous room.** The old host's room closes when it leaves. The new host opens a room
  under its own key on its own configured `veil-node`, or runs its own host-run roles. A node
  that lists allowed `hosts` may refuse the successor; that is a failed `HandoverReady`.
- **Invites.** Invites and short links are signed by the old host and hold its one-time
  tokens, so they stop working. They are not copied, because that would move the old host's
  secrets. The new host makes new invites. The leaving host's dialog and the new host's notice
  both say "Earlier invite links no longer work."
- **Removed and declined people.** The new host gets `removed` (device keys only, which
  members could already see). If one of them asks to join with a new invite, the new host's
  lobby marks them "Removed earlier in this call" and admitting them needs a second confirm.
- **Moderators.** The list is cleared. The new host names its own moderators.
- **Host-run relay and forwarder.** They stop with the old host. Media through the old host's
  forwarder falls back to mesh or relayed paths until the new host's plan passes preflight.
  With more members than mesh supports, video may drop to the Auto floor for a few seconds.
- **Relay-only members.** Today the relay operator sees a relay-only member's address, and with
  host-run roles that operator is the host. If the new host's relay is a different machine from
  the one this member used, the member's app asks before dialling: "Your relay changes to
  Ana's computer. Ana will be able to see your IP address. Stay in the call?" with **Stay** and
  **Leave**. With no answer in 20 seconds the member leaves. If both hosts use the same
  configured relay node (same node key), nothing is asked.

## 10. Later work, not in this pass

- **Unplanned host loss.** Needs an election rule that is safe under partitions, for example
  an ordered successor list fixed in the group context plus a quorum or a trusted
  coordinator. Until then a crashed host ends the call.
- **Keeping invite links working** across a handover, for example through a rendezvous name
  that is not tied to one host key.
- **Moving moderators** to the new host.

## Considered options

- **Copy the host's key and state to the successor.** Rejected: it shares a private identity
  key, breaks safety codes and makes the old device a permanent host.
- **Any member may commit (MLS external commits or member commits).** Rejected for this pass:
  it needs ordering of concurrent commits, which is the election problem. One committer per
  epoch keeps today's model.
- **Successor named in a signed app message, outside MLS.** Rejected: it would not be bound to
  an epoch, so fencing would need a second ordering scheme.
- **Ask the successor to accept at handover time.** Rejected: the leaving host would wait on
  someone who stepped away. The Available to host switch is the consent, given in advance.
- **Members send per-peer addresses or RTTs to the host for the pick.** Rejected: it would leak
  relationships and relay-only members' paths for little gain over coarse self-reports.
- **Signed group context extension and single successor** (chosen).

## Tests that ship with the code

- `authority.rs`: a commit from the old host after E+1 is refused; an unknown successor is
  refused; a non-persistent group keeps the creator as host.
- `successor.rs`: the sort order, ties, relay-only and unavailable members never chosen.
- Host and member actors: full handover with three members; successor fails before commit
  (next one is tried); successor fails after commit (members report lost); a stale `Ended`,
  plan or ticket from the old host is ignored; a removed device cannot `Resume` at the new host;
  a waiting joiner gets `HostChanged`; a relay-only member is never offered an introduction.
- Real-device evidence for planned handover on LAN and over the internet, recorded in the
  roadmap.
