//! Discovery of loopback TCP listeners by parsing `/proc/net/tcp` and
//! `/proc/net/tcp6` from inside the container.
//!
//! ## Why /proc and not `ss`
//!
//! The container image (`container/Dockerfile`) ships neither `iproute2` (`ss`)
//! nor `net-tools` (`netstat`) nor `lsof`. `/proc/net/tcp{,6}` is part of procfs
//! and needs no package at all, so discovery works in the stock image and in any
//! snapshot derived from it.
//!
//! ## Wire format
//!
//! Both files are fixed-column text with a header line:
//!
//! ```text
//!   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
//!    0: 0100007F:8707 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 27764798 1 ...
//! ```
//!
//! Only two columns matter: `local_address` (index 1) and `st` (index 3).
//! `st == 0A` is `TCP_LISTEN`; every other state is a connection, not a listener.
//!
//! ## Hex and endianness
//!
//! `local_address` is `<address>:<port>`, both hex, but they are *not* encoded
//! the same way:
//!
//! * The **port** is a plain big-endian `%04X` — `8707` is 34567.
//! * The **address** is printed as one `%08X` per 32-bit word *in host byte
//!   order*, which is little-endian on every platform this app targets. So each
//!   8-hex-digit group must be parsed as a `u32` and then expanded with
//!   [`u32::to_le_bytes`] to recover the address bytes in network order:
//!   `0100007F` → `0x0100007F` → `[7F, 00, 00, 01]` → `127.0.0.1`.
//!
//! IPv4 rows have one such group (8 hex digits); IPv6 rows have four (32 hex
//! digits), each converted independently, in order, to fill the 16 address
//! bytes. `::1` is therefore `00000000000000000000000001000000`, and the
//! IPv4-mapped `::ffff:127.0.0.1` is `0000000000000000FFFF00000100007F`.
//!
//! ## What counts as loopback
//!
//! Only `127.0.0.0/8` and `::1` (plus IPv4-mapped loopback, reported as v4).
//! A `0.0.0.0` or `::` listener is a service deliberately published to the
//! outside world — that is the port-mappings feature's job, not the auth
//! bridge's — so those rows are dropped.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// The `st` column value for `TCP_LISTEN`.
const TCP_LISTEN: &str = "0A";

/// Which loopback address family (or families) a container-side listener was
/// found on. Determines the `socat` target address used to reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortFamily {
    /// Only `127.0.0.0/8`.
    V4,
    /// Only `::1`. Common in practice: Node resolves `localhost` to IPv6 first
    /// on Linux, so `claude login` frequently binds `::1` and nothing else
    /// (anthropics/claude-code#44844).
    V6,
    /// Both — reachable either way; we use IPv4.
    Dual,
}

impl PortFamily {
    fn merge(self, other: PortFamily) -> PortFamily {
        if self == other {
            self
        } else {
            PortFamily::Dual
        }
    }

    /// The `socat` address that reaches this listener from inside the container.
    /// A `::1`-only listener genuinely cannot be reached via `127.0.0.1`
    /// (verified: connect gets ECONNREFUSED), hence the split.
    pub fn socat_target(&self, port: u16) -> String {
        match self {
            PortFamily::V4 | PortFamily::Dual => format!("TCP:127.0.0.1:{}", port),
            PortFamily::V6 => format!("TCP6:[::1]:{}", port),
        }
    }
}

/// One parsed LISTEN row that survived the loopback filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoopbackListener {
    pub port: u16,
    pub family: PortFamily,
}

/// Parse the concatenated contents of `/proc/net/tcp` and `/proc/net/tcp6` into
/// the set of loopback ports being listened on, keyed by port with the families
/// merged (a port bound on both `127.0.0.1` and `::1` yields
/// [`PortFamily::Dual`]).
///
/// Unparseable lines — the two header lines, `cat`'s "No such file" complaint
/// when IPv6 is disabled, anything else that ends up interleaved in the exec's
/// combined output — are silently ignored rather than failing the whole poll.
pub fn parse_loopback_listeners(text: &str) -> BTreeMap<u16, PortFamily> {
    let mut ports: BTreeMap<u16, PortFamily> = BTreeMap::new();
    for listener in parse_listener_rows(text) {
        ports
            .entry(listener.port)
            .and_modify(|f| *f = f.merge(listener.family))
            .or_insert(listener.family);
    }
    ports
}

/// Row-level parse, before per-port family merging. Split out so tests can
/// assert on the individual rows.
pub fn parse_listener_rows(text: &str) -> Vec<LoopbackListener> {
    text.lines().filter_map(parse_listener_row).collect()
}

fn parse_listener_row(line: &str) -> Option<LoopbackListener> {
    let mut fields = line.split_whitespace();
    let _sl = fields.next()?;
    let local_address = fields.next()?;
    let _rem_address = fields.next()?;
    let state = fields.next()?;

    if state != TCP_LISTEN {
        return None;
    }

    let (addr_hex, port_hex) = local_address.split_once(':')?;
    // The port is a straightforward big-endian hex u16 — no byte swapping.
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if port == 0 {
        return None;
    }

    let family = match addr_hex.len() {
        8 => {
            let addr = Ipv4Addr::from(parse_le_word(addr_hex)?);
            addr.is_loopback().then_some(PortFamily::V4)
        }
        32 => {
            let mut octets = [0u8; 16];
            for (i, group) in addr_hex.as_bytes().chunks(8).enumerate() {
                let group = std::str::from_utf8(group).ok()?;
                octets[i * 4..i * 4 + 4].copy_from_slice(&parse_le_word(group)?);
            }
            let addr = Ipv6Addr::from(octets);
            // An IPv4-mapped row describes a v4 socket, so it is reachable at
            // 127.0.0.1 and must be classified as v4, not v6.
            match addr.to_ipv4_mapped() {
                Some(v4) => v4.is_loopback().then_some(PortFamily::V4),
                None => addr.is_loopback().then_some(PortFamily::V6),
            }
        }
        _ => None,
    }?;

    Some(LoopbackListener { port, family })
}

/// Parse one `%08X` procfs address word into its four address bytes in network
/// order. The kernel prints the word in host byte order, so the recovered bytes
/// are the little-endian expansion of the parsed integer.
fn parse_le_word(hex: &str) -> Option<[u8; 4]> {
    Some(u32::from_str_radix(hex, 16).ok()?.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `cat /proc/net/tcp` from a running `triple-c:latest` container
    /// with three listeners deliberately started:
    ///   * `socat TCP4-LISTEN:34567,bind=127.0.0.1`  → row 0 (`0100007F:8707`)
    ///   * `socat TCP4-LISTEN:34569,bind=0.0.0.0`    → row 1 (`00000000:8709`)
    ///   * `node ... .listen(34568, "::1")`          → appears in TCP6 only
    const REAL_PROC_NET_TCP: &str = concat!(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode                                                     \n",
        "   0: 0100007F:8707 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 27764798 1 0000000000000000 100 0 0 10 0                  \n",
        "   1: 00000000:8709 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 27758875 1 0000000000000000 100 0 0 10 0                  \n",
    );

    /// Verbatim `cat /proc/net/tcp6` from the same container. The single row is
    /// the Node listener bound to `::1` only — the case that motivates the
    /// TCP6 socat target.
    const REAL_PROC_NET_TCP6: &str = concat!(
        "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
        "   0: 00000000000000000000000001000000:8708 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 27747129 1 0000000000000000 100 0 0 10 0\n",
    );

    fn both_files() -> String {
        format!("{}{}", REAL_PROC_NET_TCP, REAL_PROC_NET_TCP6)
    }

    #[test]
    fn parses_ipv4_loopback_row_with_little_endian_address() {
        let rows = parse_listener_rows(REAL_PROC_NET_TCP);
        // 0100007F → 127.0.0.1 (kept), 00000000 → 0.0.0.0 (dropped).
        assert_eq!(
            rows,
            vec![LoopbackListener {
                port: 0x8707,
                family: PortFamily::V4
            }]
        );
        assert_eq!(rows[0].port, 34567);
    }

    #[test]
    fn parses_ipv6_loopback_row() {
        let rows = parse_listener_rows(REAL_PROC_NET_TCP6);
        assert_eq!(
            rows,
            vec![LoopbackListener {
                port: 34568,
                family: PortFamily::V6
            }]
        );
    }

    #[test]
    fn ignores_wildcard_bind_addresses() {
        // 0.0.0.0:34569 is in the fixture and must never be bridged — that is
        // the port-mappings feature's territory.
        let ports = parse_loopback_listeners(&both_files());
        assert!(!ports.contains_key(&34569));

        // Same for the IPv6 wildcard and a non-loopback unicast address.
        let wildcard_v6 = "   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0 100 0 0 10 0";
        let lan_v4 = "   0: 0245A8C0:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0 100 0 0 10 0";
        assert!(parse_listener_rows(wildcard_v6).is_empty());
        assert!(parse_listener_rows(lan_v4).is_empty());
    }

    #[test]
    fn parses_both_files_concatenated_as_one_exec_output() {
        let ports = parse_loopback_listeners(&both_files());
        assert_eq!(ports.len(), 2);
        assert_eq!(ports.get(&34567), Some(&PortFamily::V4));
        assert_eq!(ports.get(&34568), Some(&PortFamily::V6));
    }

    #[test]
    fn merges_families_for_a_dual_stack_port() {
        let dual = format!(
            "{}   1: 00000000000000000000000001000000:8707 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2 1 0 100 0 0 10 0\n",
            both_files()
        );
        let ports = parse_loopback_listeners(&dual);
        assert_eq!(ports.get(&34567), Some(&PortFamily::Dual));
    }

    #[test]
    fn ipv4_mapped_loopback_is_reported_as_v4() {
        // ::ffff:127.0.0.1 — a v4 socket surfacing in /proc/net/tcp6.
        let row = "   0: 0000000000000000FFFF00000100007F:8707 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0 100 0 0 10 0";
        assert_eq!(
            parse_listener_rows(row),
            vec![LoopbackListener {
                port: 34567,
                family: PortFamily::V4
            }]
        );
    }

    #[test]
    fn ignores_non_listen_states() {
        // Same loopback address, state 01 (ESTABLISHED) instead of 0A.
        let established = "   0: 0100007F:8707 0100007F:C350 01 00000000:00000000 00:00000000 00000000     0        0 1 1 0 100 0 0 10 0";
        assert!(parse_listener_rows(established).is_empty());
    }

    #[test]
    fn ignores_headers_and_garbage() {
        assert!(parse_listener_rows("").is_empty());
        assert!(parse_listener_rows(
            "cat: /proc/net/tcp6: No such file or directory\n\n  sl  local_address rem_address   st\n"
        )
        .is_empty());
        // Truncated / malformed rows must not panic or be accepted.
        assert!(parse_listener_rows("   0: 0100007F 00000000:0000 0A").is_empty());
        assert!(parse_listener_rows("   0: ZZZZZZZZ:8707 00000000:0000 0A x").is_empty());
        assert!(parse_listener_rows("   0: 0100007F:0000 00000000:0000 0A x").is_empty());
    }

    #[test]
    fn socat_target_matches_family() {
        assert_eq!(
            PortFamily::V4.socat_target(34567),
            "TCP:127.0.0.1:34567"
        );
        assert_eq!(
            PortFamily::Dual.socat_target(34567),
            "TCP:127.0.0.1:34567"
        );
        assert_eq!(PortFamily::V6.socat_target(34568), "TCP6:[::1]:34568");
    }
}
