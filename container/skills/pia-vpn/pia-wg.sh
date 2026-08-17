#!/usr/bin/env bash
# PIA over WireGuard, headless.
#
# PIA's desktop client (pia-daemon + piactl) cannot work here: its daemon never
# accepts a client connection without the GUI running, and `piactl --help` says
# as much. This talks to PIA's public API directly instead, which is the path
# PIA themselves document for headless use.
#
#   sudo pia-wg.sh up          tunnel up, only 1.1.1.1 routed through it (safe test)
#   sudo pia-wg.sh up --full   tunnel up, all *public* traffic exits via PIA
#   sudo pia-wg.sh down        tear down, restoring DNS and routes
#   sudo pia-wg.sh status      handshake, DNS and current public IP
#
# Requires the project's "VPN support" setting (Config -> Runtime) to be on.
#
#   PIA_CREDS   credentials file, two lines: username, then password
#               (default ~/pia-creds; never echoed by this script)
#   PIA_REGION  region id (default us_chicago). List them with:
#     curl -s https://serverlist.piaservers.net/vpninfo/servers/v6 \
#       | head -1 | jq -r '.regions[].id'
set -euo pipefail

CREDS=${PIA_CREDS:-/home/claude/pia-creds}
REGION=${PIA_REGION:-us_chicago}
IFACE=pia0
STATE=/run/pia-wg

# Kept off the tunnel in --full mode. The container's DNS resolver, the Docker
# host network (host.docker.internal, any host-side Ollama), sibling containers
# and the LAN all live in here. PIA cannot route any of it, so without these
# exclusions the container reaches the public internet and nothing else --
# including, fatally, its own resolver.
PRIVATE_NETS="10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16"

# Args are joined with spaces so a long message can be written as several
# source lines without the indentation ending up in the output.
die() { echo "pia-wg: $*" >&2; exit 1; }

preflight() {
  [ "$(id -u)" = 0 ] || die "run with sudo"
  # CAP_NET_ADMIN is bit 12. Checking it by name gives a usable error; without
  # it the first `ip` call fails with a bare "Operation not permitted" that
  # points nowhere near the setting that actually needs changing.
  local caps
  caps=$(awk '/^CapEff:/{print $2}' /proc/self/status)
  if [ $(( 0x$caps & 0x1000 )) -eq 0 ]; then
    die "this container has no CAP_NET_ADMIN." \
        "Turn on \"VPN support\" in Config -> Runtime and start the project" \
        "again. That recreates the container; the home and .claude volumes" \
        "are preserved, so nothing in them is lost."
  fi
  [ -e /dev/net/tun ] || \
    die "/dev/net/tun is missing." \
        "Same fix: turn on \"VPN support\" in Config -> Runtime. If it is" \
        "already on, the Docker host's kernel is missing the tun module."
  command -v wg >/dev/null || die "wireguard-tools is not installed."
  [ -r "$CREDS" ] || \
    die "no credentials at $CREDS." \
        "Two lines are expected: username, then password." \
        "Set PIA_CREDS to read them from somewhere else."
}

# Record every route we add so teardown removes exactly those and nothing else.
add_route() { ip route add $1 2>/dev/null && echo "$1" >> "$STATE/routes" || true; }

up() {
  preflight
  mkdir -p "$STATE"; cd "$STATE"
  [ -f ca.rsa.4096.crt ] || curl -sf -m 20 -o ca.rsa.4096.crt \
    https://raw.githubusercontent.com/pia-foss/manual-connections/master/ca.rsa.4096.crt \
    || die "could not fetch PIA's CA certificate"

  local u p tok srv sip scn priv pub resp ep gw dns
  u=$(sed -n 1p "$CREDS"); p=$(sed -n 2p "$CREDS")
  tok=$(curl -sf -m 25 -u "$u:$p" \
    https://www.privateinternetaccess.com/gtoken/generateToken | jq -r .token)
  [ -n "$tok" ] && [ "$tok" != null ] || die "PIA authentication failed - check $CREDS"

  curl -sf -m 30 https://serverlist.piaservers.net/vpninfo/servers/v6 | head -1 > servers.json
  srv=$(jq -r --arg r "$REGION" '.regions[] | select(.id==$r) | .servers.wg[0]' servers.json)
  sip=$(echo "$srv" | jq -r .ip); scn=$(echo "$srv" | jq -r .cn)
  [ -n "$sip" ] && [ "$sip" != null ] || die "no WireGuard server for region $REGION"

  priv=$(wg genkey); pub=$(echo "$priv" | wg pubkey)
  printf '%s' "$priv" > wg.priv; chmod 600 wg.priv

  # PIA pins its certificate to the server's common name, which is why this
  # connects by CN and lets --connect-to point that name at the real address.
  resp=$(curl -sf -m 25 -G --connect-to "$scn::$sip:" --cacert ca.rsa.4096.crt \
    --data-urlencode "pt=$tok" --data-urlencode "pubkey=$pub" \
    "https://$scn:1337/addKey")
  [ "$(echo "$resp" | jq -r .status)" = OK ] || die "key registration failed: $resp"

  : > "$STATE/routes"
  ip link del "$IFACE" 2>/dev/null || true
  ip link add "$IFACE" type wireguard
  wg set "$IFACE" private-key wg.priv \
    peer "$(echo "$resp" | jq -r .server_key)" \
    endpoint "$(echo "$resp" | jq -r .server_ip):$(echo "$resp" | jq -r .server_port)" \
    allowed-ips 0.0.0.0/0 persistent-keepalive 25
  ip addr add "$(echo "$resp" | jq -r .peer_ip)/32" dev "$IFACE"
  ip link set "$IFACE" up

  if [ "${1:-}" = "--full" ]; then
    # Pin the endpoint to the pre-existing gateway first, so the tunnel's own
    # packets do not try to route through the tunnel. Then beat the default
    # route with two half-routes rather than replacing it -- nothing to restore
    # on teardown, and the container keeps working if this script dies midway.
    ep=$(echo "$resp" | jq -r .server_ip)
    gw=$(ip route show default | awk '{print $3; exit}')
    add_route "$ep/32 via $gw"
    add_route "0.0.0.0/1 dev $IFACE"
    add_route "128.0.0.0/1 dev $IFACE"

    # Keep container, host and LAN traffic off the tunnel. Longer prefixes than
    # the two halves above, so these win.
    for n in $PRIVATE_NETS; do add_route "$n via $gw"; done

    # PIA's resolver lives inside 10/8, so pin it back through the tunnel with a
    # /32 -- longer still, so it beats the 10.0.0.0/8 exclusion just added.
    # Using PIA's resolver rather than the container's keeps DNS from leaking,
    # and the container's own resolver is unreachable from inside the tunnel.
    dns=$(echo "$resp" | jq -r '.dns_servers[]? // empty' | head -2)
    if [ -n "$dns" ]; then
      cp /etc/resolv.conf "$STATE/resolv.conf.bak"
      for d in $dns; do add_route "$d/32 dev $IFACE"; done
      # resolv.conf is a bind mount: write through it, never replace it.
      for d in $dns; do echo "nameserver $d"; done > /etc/resolv.conf
    else
      echo "pia-wg: warning - PIA returned no DNS servers; leaving resolv.conf alone" >&2
    fi
    echo "full tunnel: public traffic exits via PIA; private ranges stay local"
  else
    add_route "1.1.1.1/32 dev $IFACE"
    echo "test route only: 1.1.1.1 goes via PIA, everything else unchanged"
  fi

  sleep 2
  status
}

down() {
  [ "$(id -u)" = 0 ] || die "run with sudo"
  if [ -f "$STATE/resolv.conf.bak" ]; then
    cat "$STATE/resolv.conf.bak" > /etc/resolv.conf
    rm -f "$STATE/resolv.conf.bak"
  fi
  if [ -f "$STATE/routes" ]; then
    # Reverse order: the specific overrides go before the ranges they sit in.
    tac "$STATE/routes" | while read -r r; do
      [ -n "$r" ] && ip route del $r 2>/dev/null || true
    done
    rm -f "$STATE/routes"
  fi
  ip link del "$IFACE" 2>/dev/null || true
  echo "tunnel down"
}

status() {
  wg show "$IFACE" 2>/dev/null | grep -E "latest handshake|transfer" || echo "no tunnel up"
  # Resolve a name, not an IP literal. A curl to 1.1.1.1 succeeds while DNS is
  # completely broken, which is exactly how a dead resolver goes unnoticed.
  printf 'DNS: '
  if timeout 10 getent hosts api.anthropic.com >/dev/null 2>&1; then
    echo "ok (via $(sed -n 's/^nameserver //p' /etc/resolv.conf | tr '\n' ' '))"
  else
    echo "BROKEN - cannot resolve api.anthropic.com"
  fi
  echo -n "public IP: "
  curl -s -m 20 https://1.1.1.1/cdn-cgi/trace | sed -n 's/^ip=//p'
}

case "${1:-}" in
  up) shift; up "${1:-}" ;;
  down) down ;;
  status) status ;;
  *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
