# Calls prefer IPv6 when both ends have it

Status: accepted (2026-11-18)

Many home and mobile connections share one IPv4 address behind carrier-grade NAT, where nobody can
dial in. The same connections often give each device a public IPv6 address. Veil now binds one
dual-stack socket where the system allows it, shares the device's public IPv6 address, and dials
IPv6 first, with IPv4 starting 250 ms later (`veil-net/src/family.rs`).

## Considered options

- **IPv4 only, with the relay for everyone it cannot connect.** Rejected. It leaves working direct
  paths unused, and sends those calls through someone's relay.
- **Separate IPv4 and IPv6 sockets.** Rejected. Hole punching needs the same socket to listen and
  dial, and two sockets would double that work.
- **IPv6 only.** Rejected. Many networks still have no IPv6.
- **One dual-stack socket, IPv6 first with a short IPv4 delay** (chosen). This follows RFC 8305
  ("Happy Eyeballs"): a working IPv6 path wins, and a broken one costs a quarter of a second.

## Consequences

- More people can see this device's public IPv6 address: the rendezvous, other members, anyone
  holding the host's invite, and, with port opening on, the router. `TRANSPARENCY.md` lists this.
  Relay-only mode still keeps a member's addresses from other members.
- Only global unicast addresses are shared. Link-local, unique local, documentation, Teredo and
  6to4 addresses are not.
- Veil finds one address, the one the system would send from. A machine with several public
  prefixes offers only that one.
- Invites keep their format. An older version skips an IPv6 address it cannot dial and uses IPv4.
- Systems that cannot make a dual-stack socket fall back to IPv4.
