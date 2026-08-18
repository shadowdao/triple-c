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
# Settings are read from the environment, but note that sudo resets it: they
# have to be passed *through* sudo, after the word `sudo`, not before it.
#
#   sudo PIA_REGION=uk_london pia-wg.sh up --full     # works
#   PIA_REGION=uk_london sudo pia-wg.sh up --full     # silently ignored
#
#   PIA_CREDS   credentials file, two lines: username, then password
#               (default /home/claude/pia-creds; never echoed by this script)
#   PIA_REGION  region id (default us_chicago). List them with:
#     curl -s https://serverlist.piaservers.net/vpninfo/servers/v6 \
#       | head -1 | jq -r '.regions[].id'
set -euo pipefail

# Not ~/pia-creds: under sudo, HOME is /root.
# Read by up()'s EXIT trap, which runs after the function's locals are gone.
SETUP_OK=0

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

# `x=$(cmd)` is a plain assignment, so `set -e` kills the script on a non-zero
# cmd *before* any `[ -z "$x" ] || die` line can run. Every capture below
# therefore goes through `run`; without it a wrong password exits 22 with no
# output at all, which is the most likely way this is used wrongly and was the
# least explained.
#
# It takes a description rather than reporting the command it ran: one of these
# invocations carries the account password in `-u`, and an error message is
# exactly the wrong place for that to surface.
run() { local what=$1; shift; "$@" || die "$what (exit $?)"; }

preflight() {
  [ "$(id -u)" = 0 ] || die "run with sudo"
  # CAP_NET_ADMIN is bit 12. Checking it by name gives a usable error; without
  # it the first `ip` call fails with a bare "Operation not permitted" that
  # points nowhere near the setting that actually needs changing.
  #
  # Deliberately NOT checking /dev/net/tun: kernel WireGuard is a netlink
  # interface and does not use it (verified -- `ip link add type wireguard`
  # succeeds with NET_ADMIN and no tun device). It is OpenVPN and userspace
  # wireguard-go that need it. The real kernel dependency here is the
  # `wireguard` module, which `ip link add` below reports on directly.
  local caps
  caps=$(awk '/^CapEff:/{print $2}' /proc/self/status)
  if [ $(( 0x$caps & 0x1000 )) -eq 0 ]; then
    die "this container has no CAP_NET_ADMIN." \
        "Turn on \"VPN support\" in Config -> Runtime and start the project" \
        "again. That recreates the container; the home and .claude volumes" \
        "are preserved, so nothing in them is lost."
  fi
  command -v wg >/dev/null || \
    die "wireguard-tools is not installed." \
        "If this project's container was built from an older base image," \
        "migrate it onto the current one -- that is what ships \`wg\`."
  [ -r "$CREDS" ] || \
    die "no credentials at $CREDS." \
        "Two lines are expected: username, then password." \
        "Set PIA_CREDS (after the word \`sudo\`) to read them elsewhere."
}

# Routes that must work. A silent failure here is the worst state this script
# can reach: the two half-routes need no gateway and would succeed, so the
# tunnel captures everything while the exclusions that keep DNS and the Docker
# host reachable are quietly missing -- and `status` still says "full tunnel".
add_route() {
  ip route add "$@" || die "could not add route '$*'"
  printf '%s\n' "$*" >> "$STATE/routes"
}

up() {
  case "${1:-}" in
    ""|--full) ;;
    *) die "unknown option '$1' (expected --full or nothing)." \
           "Refusing rather than silently giving you a test route." ;;
  esac
  preflight

  mkdir -p "$STATE"; cd "$STATE"

  # `curl -o` creates the file before it knows the request failed, so a plain
  # `[ -f ]` cache check can pin a truncated cert forever -- and /run rides the
  # snapshot, so "forever" outlives the container. Fetch to a temp name and
  # rename only on success.
  if [ ! -s ca.rsa.4096.crt ]; then
    run "could not download PIA's CA certificate" \
      curl -sf -m 20 -o ca.crt.part \
      https://raw.githubusercontent.com/pia-foss/manual-connections/master/ca.rsa.4096.crt
    [ -s ca.crt.part ] || die "PIA's CA certificate downloaded empty"
    mv ca.crt.part ca.rsa.4096.crt
  fi

  local u p tok srv sip scn priv pub resp ep gw dns
  u=$(sed -n 1p "$CREDS"); p=$(sed -n 2p "$CREDS")
  [ -n "$u" ] && [ -n "$p" ] || die "$CREDS needs two lines: username, then password"

  # Via stdin, not `-u`. curl does blank the password in its own argv, but only
  # once it is running: sampling /proc/<pid>/cmdline in a tight loop caught the
  # plaintext in 3 of 200 tries, in the window between exec and the overwrite.
  # Small, but this is the permanent account password, and the mechanism to
  # avoid it entirely is already here for the token.
  tok=$(printf -- '--user "%s:%s"\n' "$u" "$p" \
    | run "PIA rejected the credentials in $CREDS, or could not be reached" \
        curl -sf -m 25 -K - \
        https://www.privateinternetaccess.com/gtoken/generateToken | jq -r .token)
  [ -n "$tok" ] && [ "$tok" != null ] || die "PIA returned no token - check the credentials in $CREDS"

  run "could not fetch PIA's server list" \
    curl -sf -m 30 https://serverlist.piaservers.net/vpninfo/servers/v6 \
    | head -1 > servers.json
  srv=$(jq -r --arg r "$REGION" '.regions[] | select(.id==$r) | .servers.wg[0]' servers.json)
  sip=$(echo "$srv" | jq -r .ip); scn=$(echo "$srv" | jq -r .cn)
  [ -n "$sip" ] && [ "$sip" != null ] || die "no WireGuard server for region '$REGION'"

  # The key is generated but NOT written yet -- `down` below deletes wg.priv, and
  # the teardown has to come after every fetch that can fail.
  priv=$(wg genkey); pub=$(printf '%s' "$priv" | wg pubkey)

  # The token goes in on stdin as a curl config rather than in the argv, where
  # `ps` and /proc/*/cmdline expose it to every process in the container --
  # verified. It is a ~24h bearer credential for the whole PIA account.
  # PIA pins its certificate to the server's common name, which is why this
  # connects by CN and lets --connect-to point that name at the real address.
  resp=$(printf -- '--data-urlencode "pt=%s"\n--data-urlencode "pubkey=%s"\n' "$tok" "$pub" \
    | run "could not register the key with $scn" \
        curl -sf -m 25 -G -K - --connect-to "$scn::$sip:" \
        --cacert ca.rsa.4096.crt "https://$scn:1337/addKey")
  [ "$(echo "$resp" | jq -r .status)" = OK ] || die "key registration failed: $resp"

  # Only now tear down any previous tunnel. Every network call above this line
  # can fail, and an earlier version tore down first -- so a failed token fetch,
  # an unreachable server list, or a refused key registration took a *working*
  # tunnel with it and silently reverted the container to its real address while
  # the error talked about credentials. Nothing above this line has touched the
  # network stack. addKey is the most failure-prone of the three: it reaches one
  # individual gateway by CN with a pinned certificate.
  #
  # It also still does the job it was added for: clearing a stale resolv.conf
  # backup so a second `up` cannot save PIA's own resolvers over the real ones.
  down >/dev/null 2>&1 || true

  # From here on the network stack is being modified, so any failure has to put
  # it back rather than exit half-configured.
  #
  # EXIT rather than ERR, and a flag rather than the trap's own exit status: an
  # ERR trap is not inherited by shell functions without `set -E`, so a failure
  # inside add_route would not fire it, and `die` exits explicitly, which is not
  # an error and would not fire it either. EXIT catches both.
  SETUP_OK=0
  trap '[ "$SETUP_OK" = 1 ] || { echo "pia-wg: setup failed - rolling back" >&2; down >/dev/null 2>&1 || true; }' EXIT

  # umask, not a later chmod: created under the inherited 0022 otherwise, so the
  # key would be world-readable for the moment in between.
  ( umask 077; printf '%s' "$priv" > wg.priv )

  : > "$STATE/routes"
  ip link add "$IFACE" type wireguard 2>/dev/null || \
    die "could not create a WireGuard interface." \
        "The Docker host's kernel has no 'wireguard' module."
  wg set "$IFACE" private-key wg.priv \
    peer "$(echo "$resp" | jq -r .server_key)" \
    endpoint "$(echo "$resp" | jq -r .server_ip):$(echo "$resp" | jq -r .server_port)" \
    allowed-ips 0.0.0.0/0 persistent-keepalive 25
  # The kernel holds the key from here, so the file has no reason to outlive
  # this line -- and every reason not to: /run is in the writable layer, and a
  # recreate or migrate runs `docker commit` over it without tearing the tunnel
  # down first, baking the key into the project's snapshot image. `down` also
  # removes it, for the case where `up` never got this far.
  rm -f wg.priv
  ip addr add "$(echo "$resp" | jq -r .peer_ip)/32" dev "$IFACE"
  ip link set "$IFACE" up

  if [ "${1:-}" = "--full" ]; then
    ep=$(echo "$resp" | jq -r .server_ip)
    gw=$(ip route show default | awk '{print $3; exit}')
    # `default dev eth0` with no `via` yields the literal "eth0" here, which
    # would make every exclusion below a malformed no-op.
    [[ $gw =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
      die "no usable default gateway to pin the tunnel against (got '${gw:-none}')"

    # PIA's resolvers are required in --full. Without them the 10/8 exclusion
    # below is already in place, so every lookup would go to the container's
    # own resolver *outside* the tunnel -- a full tunnel leaking all its DNS,
    # reported by `status` as perfectly healthy.
    dns=$(echo "$resp" | jq -r '.dns_servers[]? // empty' | head -2)
    [ -n "$dns" ] || die "PIA returned no DNS servers; refusing a full tunnel that would leak every lookup"

    # Pin the endpoint to the pre-existing gateway first, so the tunnel's own
    # packets do not try to route through the tunnel. Then beat the default
    # route with two half-routes rather than replacing it -- nothing to restore
    # on teardown, and the container keeps working if this script dies midway.
    add_route "$ep/32" via "$gw"
    add_route 0.0.0.0/1 dev "$IFACE"
    add_route 128.0.0.0/1 dev "$IFACE"

    # Keep container, host and LAN traffic off the tunnel. Longer prefixes than
    # the two halves above, so these win.
    for n in $PRIVATE_NETS; do add_route "$n" via "$gw"; done

    # PIA's resolvers live inside 10/8, so pin them back through the tunnel with
    # /32s -- longer still, so they beat the exclusion just added.
    # `host.docker.internal` is answered only by the resolver about to be
    # replaced -- it is not in /etc/hosts. Triple-C hands that name to the
    # container for the LiteLLM gateway and defaults host-side Ollama and custom
    # endpoints to it, so losing it takes the project's model backend with it.
    # The *route* to it is already excluded above; only the name needs pinning.
    # Resolve it with the old resolver and write it into /etc/hosts first.
    local hdi
    hdi=$(getent ahostsv4 host.docker.internal 2>/dev/null | awk '{print $1; exit}')
    if [ -n "$hdi" ]; then
      cp /etc/hosts "$STATE/hosts.bak"
      printf '%s host.docker.internal\n' "$hdi" >> /etc/hosts
    fi

    cp /etc/resolv.conf "$STATE/resolv.conf.bak"
    for d in $dns; do add_route "$d/32" dev "$IFACE"; done
    # resolv.conf is a bind mount: write through it, never replace it.
    for d in $dns; do echo "nameserver $d"; done > /etc/resolv.conf
    echo "full tunnel: public traffic exits via PIA; private ranges stay local"
  else
    add_route 1.1.1.1/32 dev "$IFACE"
    echo "test route only: 1.1.1.1 goes via PIA, everything else unchanged"
  fi

  # A tunnel with no handshake still routes -- into a black hole. Without this
  # `up --full` would exit 0 having pointed all traffic *and* resolv.conf at a
  # peer that never answered, and `status` would print "mode: full tunnel".
  # Demand a number, not just "different from 0". `wg show` prints nothing at
  # all when the interface has no peer, and writes to stderr when the interface
  # is gone -- both leave $2 empty, and `[ "" != 0 ]` is true, so the original
  # form treated a missing tunnel as a completed handshake and exited 0. `until`
  # suspends both `set -e` and `pipefail`, so nothing else was going to catch it.
  local waited=0 hs
  until hs=$(wg show "$IFACE" latest-handshakes 2>/dev/null | awk 'NR==1{print $2}')
        [[ $hs =~ ^[0-9]+$ ]] && [ "$hs" -gt 0 ]; do
    waited=$((waited + 1))
    [ "$waited" -lt 40 ] || die "no handshake from $REGION after 20s"
    sleep 0.5
  done

  SETUP_OK=1
  trap - EXIT
  status
}

down() {
  [ "$(id -u)" = 0 ] || die "run with sudo"
  # Teardown must finish even if a step fails; a half-rollback is the state this
  # exists to prevent. Deliberately not inherited from the caller's `set -e`.
  set +e
  # First, because removing the interface removes every route that points at it.
  ip link del "$IFACE" 2>/dev/null
  # Only restore something that actually looks like a resolver file. Restoring
  # an empty or truncated backup leaves the container with no DNS at all, which
  # is worse than leaving the current one alone.
  if [ -f "$STATE/resolv.conf.bak" ]; then
    if grep -q '^nameserver' "$STATE/resolv.conf.bak" 2>/dev/null; then
      cat "$STATE/resolv.conf.bak" > /etc/resolv.conf
    else
      echo "pia-wg: warning - saved resolv.conf looks empty; leaving the current one alone" >&2
    fi
    rm -f "$STATE/resolv.conf.bak"
  fi
  if [ -f "$STATE/hosts.bak" ]; then
    cat "$STATE/hosts.bak" > /etc/hosts
    rm -f "$STATE/hosts.bak"
  fi
  if [ -f "$STATE/routes" ]; then
    # Reverse order: the specific overrides go before the ranges they sit in.
    tac "$STATE/routes" | while read -r r; do
      [ -n "$r" ] && ip route del $r 2>/dev/null || true
    done
    rm -f "$STATE/routes"
  fi
  # /run is in the writable layer and `docker commit` bakes it into the
  # project's snapshot image, so a key left here rides that image into every
  # future container. Verified: a snapshot already carried one.
  rm -f "$STATE/wg.priv"
  echo "tunnel down"
}

# Both are Cloudflare and both answer /cdn-cgi/trace over their bare address, so
# neither needs DNS. Only 1.1.1.1 is ever routed into the tunnel, which is what
# lets status tell the two exits apart.
TRACE_TUNNELLED=https://1.1.1.1/cdn-cgi/trace
TRACE_DIRECT=https://1.0.0.1/cdn-cgi/trace

exit_ip() { curl -s -m 20 "$1" | sed -n 's/^ip=//p'; }

status() {
  # `wg show` needs root; `ip route`/`ip link` do not. Without this guard an
  # unprivileged run prints "no tunnel up" and then "mode: full tunnel" in the
  # same breath, and an agent reading the first line re-runs `up`.
  [ "$(id -u)" = 0 ] || die "run with sudo"
  wg show "$IFACE" 2>/dev/null | grep -E "latest handshake|transfer" || echo "no tunnel up"

  # Resolve a name, not an IP literal. A curl to 1.1.1.1 succeeds while DNS is
  # completely broken, which is exactly how a dead resolver goes unnoticed.
  printf 'DNS: '
  if ! timeout 10 getent hosts api.anthropic.com >/dev/null 2>&1; then
    echo "BROKEN - cannot resolve api.anthropic.com"
  elif ! timeout 10 getent hosts host.docker.internal >/dev/null 2>&1; then
    # PIA's resolvers answer public names happily, so probing only
    # api.anthropic.com reports "ok" on a container that has just lost the
    # Docker host -- and with it the LiteLLM gateway and any host-side Ollama.
    echo "public ok, but host.docker.internal is UNRESOLVABLE (gateway/Ollama backends will fail)"
  else
    echo "ok (via $(sed -n 's/^nameserver //p' /etc/resolv.conf | tr '\n' ' '))"
  fi

  # Report the exit per mode. In test mode the probe address is itself the one
  # thing inside the tunnel, so a single "public IP" line would print a PIA
  # address while every other packet leaves directly -- the exact reading that
  # makes a test tunnel look like a full one.
  if ip route show 0.0.0.0/1 2>/dev/null | grep -q "$IFACE"; then
    echo "mode: full tunnel"
    echo "  all traffic exits: $(exit_ip "$TRACE_TUNNELLED")"
  elif ip link show "$IFACE" >/dev/null 2>&1; then
    echo "mode: test route only (1.1.1.1 through the tunnel, nothing else)"
    echo "  through the tunnel: $(exit_ip "$TRACE_TUNNELLED")"
    echo "  everything else:    $(exit_ip "$TRACE_DIRECT")   <- your real address"
  else
    echo "mode: no tunnel"
    echo "  all traffic exits: $(exit_ip "$TRACE_DIRECT")"
  fi
}

case "${1:-}" in
  up) shift; up "${1:-}" ;;
  down) down ;;
  status) status ;;
  *) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
