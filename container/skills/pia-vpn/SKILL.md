---
name: pia-vpn
description: Connect this container's traffic through a PIA VPN tunnel over WireGuard, or diagnose one that is not working. Use when asked to enable, route through, check, or tear down a VPN, when traffic needs to leave from a different location, or when DNS or connectivity broke after a VPN was brought up.
---

# PIA VPN

Bring this container's traffic out through Private Internet Access over
WireGuard, using the API PIA documents for headless use.

Run `sudo ~/.claude/skills/pia-vpn/pia-wg.sh` with `up`, `up --full`, `down` or
`status`. Read the rest of this page before the first `up --full` — three of the
behaviours below are actively misleading if you meet them without warning, and
each one presents as "the VPN is fine" or "Claude is broken" rather than as
what it is.

## Before anything else: what the toggle does not do

Triple-C's **VPN support** setting grants three things — `CAP_NET_ADMIN`, the
`/dev/net/tun` device, and the `net.ipv4.conf.all.src_valid_mark` sysctl — and
stops there. It starts no client, builds no tunnel and changes no route.

So "the VPN is enabled but traffic isn't going through it" is normally not a
fault. It means the capability is present and nothing has used it yet. Check
with `status` before assuming something is broken.

If the toggle is off, the script says so and names the setting. It cannot be
turned on from inside the container; the user changes it in Config → Runtime,
and it recreates the container on the next start (home and `.claude` volumes
are preserved — it is not a Reset).

## Two modes

| | routes | use when |
|---|---|---|
| `up` | only `1.1.1.1/32` | verifying the tunnel works without disturbing anything |
| `up --full` | all public traffic | you actually want traffic leaving via PIA |

Prefer `up` first. It proves the handshake, credentials and region are good
while your own connectivity is untouched, so a failure is cheap.

**`up --full` routes Claude Code's own API traffic through PIA.** If the tunnel
drops, that traffic stops until it recovers or you run `down`. Say so before
running it — the user may be mid-session, and they will experience the failure
as Claude going away, not as a VPN problem.

## Trap 1: a full tunnel takes DNS with it

The container resolves through an address on the Docker network — under Docker
Desktop, `192.168.65.7` — which sits **outside** the container's own subnet. A
default route of `0.0.0.0/0`, or the `0.0.0.0/1` + `128.0.0.0/1` pair, captures
it and posts every lookup into a tunnel that cannot carry private traffic.

Nothing resolves after that. The visible symptom is Claude Code reporting it
cannot connect, because `api.anthropic.com` no longer resolves:

```
$ curl https://api.anthropic.com/v1/messages
* Could not resolve host: api.anthropic.com     (rc=6)
```

`pia-wg.sh` already handles this: it routes `10.0.0.0/8`, `172.16.0.0/12`,
`192.168.0.0/16` and `169.254.0.0/16` back via the original gateway, then pins
PIA's own resolvers through the tunnel with `/32` routes that outrank the
`10/8` exclusion. If you ever route traffic by hand, you owe both halves — the
exclusions *and* a resolver reachable from wherever you pointed the default.

The failure has a quiet twin. Do only the first half — exclude the private
ranges, leave the resolver alone — and everything *works*, while every DNS
query travels outside the tunnel to your ISP. A VPN that leaks the full list of
what you looked up is worse than one that is visibly broken, so `up --full`
refuses to proceed if PIA does not hand back resolvers rather than carrying on
without them.

The mechanism above is Docker Desktop's. On a user-defined Docker network the
resolver is `127.0.0.11`, which is loopback and never captured by a default
route — the trap still exists there (that resolver forwards upstream from
inside the container's namespace) but arrives by a different path. Check
`/etc/resolv.conf` rather than assuming which case you are in.

## Trap 2: an IP-literal health check cannot see a dead resolver

`curl https://1.1.1.1/cdn-cgi/trace` needs no DNS, so it returns a cheerful
PIA exit address while name resolution is entirely broken. A tunnel verified
that way looks perfect and works for nothing.

`status` resolves a real name for this reason. Trust its `DNS:` line, and if
you check by hand, resolve a name rather than fetching an address.

## Trap 3: in test mode, the obvious probe is the one thing tunnelled

`up` routes `1.1.1.1` and nothing else. So checking your address by fetching
`https://1.1.1.1/cdn-cgi/trace` reports a **PIA** address — not because your
traffic is going through PIA, but because that single probe is. Everything else
still leaves directly.

This reads exactly like a working full tunnel, and it is the likeliest reason
someone concludes the VPN is on when it is not. `status` prints both exits in
test mode for this reason:

```
mode: test route only (1.1.1.1 through the tunnel, nothing else)
  through the tunnel: 64.113.5.73
  everything else:    172.116.197.166   <- your real address
```

Two different addresses there is correct and expected in test mode. If you want
the second line to change, you want `up --full`.

## Trap 4: no tunnel survives a restart, and it fails open

The network namespace is rebuilt every time the container starts, and nothing
inside reconnects anything. After a stop/start, Reset or any config change that
recreates the container, the interface and its routes are gone.

State under `/run/pia-wg` rides the snapshot and persists, so leftover files
make it look as though the tunnel is still configured. It is not. Traffic goes
out the real address with no error and nothing visibly different.

Never infer from `/run/pia-wg` that a tunnel is up. Run `status` — if the
handshake line is missing, there is no tunnel. Re-run `up` after every start.

## Credentials

Two lines in `~/pia-creds` — username, then password:

```
p1234567
your-password
```

Treat the contents as secret: never print the file, never echo the values, and
never include them in a commit, a log or a message. The script reads it directly
and does not echo it, and passes PIA's session token to `curl` on stdin rather
than in the argv, where `ps` would expose it to everything in the container.

`PIA_CREDS` points somewhere else — but `sudo` resets the environment, so it
only takes effect **after** the word `sudo`:

```bash
sudo PIA_CREDS=/path/to/creds ~/.claude/skills/pia-vpn/pia-wg.sh up   # works
PIA_CREDS=/path/to/creds sudo ~/.claude/skills/pia-vpn/pia-wg.sh up   # ignored
```

The second form fails silently back to the default path. Same for `PIA_REGION`.

## Regions

Defaults to `us_chicago`. Override with `PIA_REGION`:

```bash
sudo PIA_REGION=uk_london ~/.claude/skills/pia-vpn/pia-wg.sh up --full
```

List the ids:

```bash
curl -s https://serverlist.piaservers.net/vpninfo/servers/v6 \
  | head -1 | jq -r '.regions[].id'
```

## Verifying

`status` prints the handshake, DNS, and which address traffic actually leaves
from — labelled by mode, so the answer cannot be misread:

```
  latest handshake: 2 seconds ago
  transfer: 92 B received, 180 B sent
DNS: ok (via 10.0.0.243 10.0.0.242)
mode: full tunnel
  all traffic exits: 64.113.5.244
```

All of it matters. A handshake with `DNS: BROKEN` is trap 1. `mode: test route
only` with two different addresses is trap 3, and is correct — it means the
tunnel works and you have not asked for it to carry anything yet. Report the
mode line when telling someone the VPN is on; "the public IP is a PIA one" is
true in test mode too, and means much less than it sounds like.

## Tearing down

`down` restores `resolv.conf` from its backup (only if that backup still looks
like a resolver file — restoring a truncated one would leave the container with
no DNS at all), removes exactly the routes that were added, in reverse order,
and deletes the interface. It is safe to run when nothing is up. Confirm
afterwards that the public address is back to the container's own.

`up` calls it too, but only *after* every network fetch has succeeded, so a
failed `up` leaves an existing tunnel alone rather than tearing it down to
report a bad password. From that point on a rollback is armed: if any step of
the setup fails, the tunnel is torn down rather than left half-configured.

The private key is deleted earlier still — the moment `wg set` has read it,
while the tunnel is being built. That is not housekeeping: `/run` is in the
container's writable layer, and recreating or migrating the project runs
`docker commit` over it *without* tearing the tunnel down first. A key that
lived for the tunnel's lifetime would be baked into the snapshot image and
copied forward from then on. The kernel keeps its own copy, so nothing is lost.

## What this deliberately does not do

- **No killswitch.** `iptables` *is* in the image, so one is buildable — this
  is a deliberate omission, not a missing dependency. Blocking non-tunnel egress
  cuts Claude Code's own API traffic the moment the tunnel drops, which ends the
  session that would otherwise fix it. If the user needs guaranteed egress
  rather than convenient egress, say so plainly and let them decide, rather than
  improvising one.
- **No autostart.** There is no service manager in the container and Triple-C
  has no start hook, so nothing re-establishes the tunnel on its own. `cron` is
  in the image and `triple-c-scheduler` runs on it, so a scheduled reconnect is
  possible if the user wants one — it is just not set up, and a tunnel that
  reconnects unattended deserves an explicit decision.
- **Not PIA's desktop client.** `pia-daemon` and `piactl` are installable but
  cannot work headless: the daemon never accepts a client connection without
  the GUI, and `piactl --help` states that connecting requires it. If you find
  one installed, it is not a working alternative to this script.
- **Not `wg-quick`.** Its `Table=auto` full-tunnel mode routes by firewall mark
  and needs `xt_CONNMARK` from the host kernel, which Docker Desktop for
  Windows (WSL2) does not have and a container cannot load. This script adds
  the routes with `ip route` directly, which works on every host.
- **IPv4 only.** The `0.0.0.0/1` + `128.0.0.0/1` pair covers v4. A container
  with a global IPv6 address and a v6 default route would leak all v6 traffic
  outside the tunnel; Triple-C's containers do not have one by default, but
  check `ip -6 route show default` before relying on this where it matters.
