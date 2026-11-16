# Security

Veil is early and has had no independent security audit. Do not trust it with anything that
matters yet. [docs/TRANSPARENCY.md](docs/TRANSPARENCY.md) lists what it does not protect.

## Report a hole

Please report it privately, not in a public issue:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**.
3. Say what you found, how to reproduce it, and what an attacker gains.

Only the maintainer sees the report. You get a reply when it has been read. This is a
one-person project, so there is no fixed response time, and no bug bounty.

## What counts

Anything that breaks what Veil promises, for example:

- reading or changing media, names or chat without being a member of the call;
- a member sending names or chat as another member;
- a removed member still sending or receiving in the call;
- a relay, forwarder or rendezvous node learning more than TRANSPARENCY.md says it can;
- a crash or memory fault from a packet anyone can send;
- getting a device key or the verified list off a machine without that user's Windows login.

Already known, and listed in TRANSPARENCY.md under "Known leaks": packet sizes and timing,
layer markers, and video and sound proving only that a member sent them, not which member.
A new way to exploit one of these is still worth reporting.

## Supported versions

Only the latest release and `main` get fixes.
