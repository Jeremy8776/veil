# A full relay shares its capacity equally between address blocks

Status: accepted (2026-11-18)

A relay has a total budget, 1 Gbit/s by default, on top of the 16 Mbit/s cap on each session. When
the budget ran out, the first packets to arrive won, so one sender could starve every other call on
that relay. Veil now keeps the budget in a `FairShare` (`veil-rendezvous/src/limit.rs`). While more
than half of the burst is left, anyone may use it. Below half, each sending address block gets an
equal part of every 100 ms window, and packets over that part are dropped.

## Considered options

- **First come, first served** (the old behaviour). Rejected. It is simple, but a flood decides who
  gets through.
- **An equal part per session or per device key.** Rejected. Both are free to create, so a sender
  could buy a larger part by opening more of them.
- **Fair queuing with a queue per sender.** Rejected for now. It divides capacity more exactly, but
  the relay would hold packets, which costs memory an attacker can aim at and adds delay to live
  audio and video.
- **Fair dropping by address block** (chosen). It needs one counter per recent sender and holds no
  packets. The end-to-end QUIC link slows down when its packets are dropped, as it would on a
  congested network.

## Consequences

- Spare capacity is not wasted: a single busy sender can still use the whole budget while nobody
  else needs it.
- Blocks are those of `veil_net::accept::block`: one IPv4 address, or one IPv6 /64. People behind
  one carrier-grade NAT address share one part when the relay is full. If busy mobile networks
  suffer from this, weigh a block by its sessions.
- The block is read for each packet, so a sender whose address changes during a call is counted
  under its new address.
- The behaviour is tested in simulated time. It has not been measured on a loaded relay.
