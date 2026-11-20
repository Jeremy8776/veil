# Self-hosting a Veil node

`veil-node` provides rendezvous, relay and forwarding roles. It holds no call encryption
keys. It can see connection metadata and can interrupt traffic, so choose who operates it
as well as who you admit to calls. [Transparency](TRANSPARENCY.md) describes those limits.

Use a node on a publicly reachable machine when a host is behind carrier-grade NAT, such
as a phone hotspot. Veil ships no default public node. A server behind the same unreachable
network does not solve that problem.

## Configure the node

Get the node binary from the matching release's Windows portable ZIP, macOS tool archive
or Linux archive, or build it with `cargo build -p veil-node --release`.

Copy [veil-node.example.toml](../crates/veil-node/veil-node.example.toml) to
`veil-node.toml`. This example enables both roles:

```toml
identity_file = "node.key"
hosts = ["REPLACE_WITH_A_HOST_DEVICE_KEY"]

[rendezvous]
listen = "[::]:4433"
relay = true

[forwarder]
listen = "[::]:4434"
max_rooms = 4096
max_rooms_per_block = 16
max_members_per_room = 16
node_queue_mib = 1024
source_queue_mib = 128
```

Replace the placeholder with the 43-character key from **Copy device key** on the host's
Home screen. List more hosts as separate strings. This is the desktop host's device key;
`node.key` is the server's own separate identity.

With `hosts` present, only those keys can host or request reachability checks; their guests
still join after host admission. Omitting `hosts` lets anyone who knows the node's address
host through it and consume its bandwidth. An empty list is refused as a configuration error.

Linux/macOS desktop keys currently change after the app restarts. Their node allowance must
then be updated. Their Connection settings also need re-entering after a standard restart;
environment variables can supply the node addresses each time. Windows persists both its
device key and Connection settings unless the person changes them.

`[::]` is a dual-stack listen address for IPv4 and IPv6. Use `0.0.0.0` for IPv4 only. Each
role needs a different port. Relative `identity_file` paths are resolved beside the config
file. The node creates that key on first start; keep it private and stable.

## Make it reachable

Allow inbound **UDP** on the configured ports in the server firewall and any cloud network
rules. If the server itself is behind a router, forward those UDP ports to it. Veil's relay
is QUIC over UDP; opening TCP 443 alone does not help.

Run:

```text
veil-node veil-node.toml
```

On Windows use `veil-node.exe` (or `./veil-node.exe` in PowerShell). The process prints a
`key@address` for each role and stays running until Ctrl+C. Use your own service manager if
it should survive a terminal closing or restart after a reboot; releases do not install a
node service automatically.

The printed address may be a listening wildcard. **Do not paste `0.0.0.0` or `[::]` into
the app.** Keep the printed key, but replace that host with the server's public address or
DNS name: `KEY@your-server.example:4433` for rendezvous and
`KEY@your-server.example:4434` for the forwarder. The key pins the actual server identity,
even when DNS resolves its address.

## Connect the desktop app

On the hosting desktop, open **Connection** from Home:

1. Put the node's public `KEY@HOST:PORT` for port 4433 in Rendezvous.
2. Put its public `KEY@HOST:PORT` for port 4434 in Forwarder if that role is enabled.
3. Save and start a new call. Its invites carry the chosen infrastructure to guests.

The two fields are separate. A forwarder handles larger calls; rendezvous/relay helps
members find each other and connect when direct paths fail. The app's current layout is
mesh through four people and forwarded above that. Guests normally just use the invite.

Environment variables override saved fields: `VEIL_RENDEZVOUS`, `VEIL_FORWARDER`,
`VEIL_ADVERTISE` (comma-separated literal `IP:PORT` addresses) and `VEIL_PORT` (a fixed
local UDP port). The Connection screen shows overrides. Node settings accept DNS names;
advertised direct addresses must be literal IP addresses.

Check People and call stats for reachability and link paths. A node refusing your host key
produces a clear error; correct the allow-list or use the device key from this run. No
forwarder cascades, federation, enterprise sign-in or TCP relay fallback are available yet.
