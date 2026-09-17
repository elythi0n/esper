//! The two wire formats, encoded and decoded by hand.
//!
//! Both are small, fixed-layout, big-endian, and spoken to one address on one
//! port. A crate for that would be more surface than the thing it wraps — and
//! this is a message sent to a device nobody here controls, parsed on the way
//! back, which is precisely the kind of code worth being able to read in one
//! sitting.
//!
//! # The two protocols, and why both
//!
//! **NAT-PMP** (RFC 6886) is Apple's, from 2005, and is what most consumer
//! routers that do anything at all will do. Twelve bytes out, sixteen back.
//!
//! **PCP** (RFC 6887) is its IETF successor and shares the port, which is not
//! a coincidence: a PCP server is required to recognise a NAT-PMP request by
//! its version byte and answer it. So one socket to one address can try the
//! newer protocol and fall back without a second round of discovery.
//!
//! PCP is tried first because it says more: it carries the external *address*
//! as well as the port, it has a nonce that makes a mapping this client's own,
//! and it works through more than one layer of NAT. NAT-PMP is the fallback
//! because it is what old routers have.

use std::net::{Ipv4Addr, SocketAddrV4};

/// The port both protocols are spoken on.
pub const PORT: u16 = 5351;

/// PCP's version byte.
const PCP_VERSION: u8 = 2;

/// NAT-PMP's version byte. **Zero, and that is the whole fallback mechanism**:
/// a PCP server sees a version it does not know and answers `UNSUPP_VERSION`,
/// while a NAT-PMP-only router sees the version it speaks.
const NATPMP_VERSION: u8 = 0;

/// PCP opcode: create or refresh a mapping.
const PCP_OP_MAP: u8 = 1;

/// The bit the server sets in the opcode byte of a response.
const PCP_RESPONSE: u8 = 0x80;

/// IANA protocol number for UDP. Esper's transport is QUIC, which is UDP.
const PROTO_UDP: u8 = 17;

/// NAT-PMP opcode: map a UDP port.
const NATPMP_OP_UDP: u8 = 1;

/// A PCP mapping nonce.
///
/// **Twelve random bytes that make the mapping this client's.** Without it,
/// anything else on the network could refresh or delete a mapping by naming
/// the same internal port — the nonce is what the router matches on, so a
/// mapping can only be changed by whoever created it.
pub type Nonce = [u8; 12];

/// What the router said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Granted {
    /// Where the world can reach this machine.
    pub external: SocketAddrV4,
    /// How long the router will keep it, before it has to be asked again.
    pub lifetime_secs: u32,
}

/// Why a request could not be understood or was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Rejected {
    /// The answer was too short to be one.
    #[error("the router answered with {got} bytes, which is not a {what} reply")]
    TooShort { what: &'static str, got: usize },
    /// The answer was not for the protocol that was asked.
    #[error("the router answered with version {got}, not {want}")]
    WrongVersion { want: u8, got: u8 },
    /// The answer was not to the question asked.
    #[error("the router answered opcode {got:#04x}, which does not answer {want:#04x}")]
    WrongOpcode { want: u8, got: u8 },
    /// The answer was to a different request.
    #[error("the router answered about a mapping this machine did not ask for")]
    NotOurs,
    /// The router understood and said no.
    #[error("the router refused: {said}")]
    Refused { code: u16, said: &'static str },
}

/// A PCP `MAP` request.
///
/// Sixty bytes: a twenty-four byte header and a thirty-six byte MAP body. The
/// layout is RFC 6887 §11.1, and every reserved field is zero because the RFC
/// says a client must send zero and a server must ignore it.
#[must_use]
pub fn pcp_request(
    client: Ipv4Addr,
    nonce: &Nonce,
    internal_port: u16,
    want_external: u16,
    lifetime_secs: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(60);
    out.push(PCP_VERSION);
    // R=0 (request) in the top bit, opcode in the low seven.
    out.push(PCP_OP_MAP);
    out.extend_from_slice(&[0, 0]); // reserved
    out.extend_from_slice(&lifetime_secs.to_be_bytes());
    // The client's own address, as an IPv4-mapped IPv6 address. The router
    // compares it against where the packet came from, and a mismatch is how it
    // notices a client behind another NAT.
    out.extend_from_slice(&client.to_ipv6_mapped().octets());
    out.extend_from_slice(nonce);
    out.push(PROTO_UDP);
    out.extend_from_slice(&[0, 0, 0]); // reserved
    out.extend_from_slice(&internal_port.to_be_bytes());
    out.extend_from_slice(&want_external.to_be_bytes());
    // A suggested external address of `::` means "any", which is what a client
    // behind one NAT wants: the router picks its own WAN address.
    out.extend_from_slice(&[0u8; 16]);
    out
}

/// Read a PCP `MAP` response, checking it answers the request that was sent.
///
/// # Errors
///
/// [`Rejected`] when the reply is malformed, is for another request, or says no.
pub fn pcp_response(bytes: &[u8], nonce: &Nonce, internal_port: u16) -> Result<Granted, Rejected> {
    // 24-byte header plus a 36-byte MAP body.
    if bytes.len() < 60 {
        return Err(Rejected::TooShort {
            what: "PCP",
            got: bytes.len(),
        });
    }
    let at = |i: usize| bytes.get(i).copied().unwrap_or(0);
    let be16 = |i: usize| u16::from_be_bytes([at(i), at(i + 1)]);
    let be32 = |i: usize| u32::from_be_bytes([at(i), at(i + 1), at(i + 2), at(i + 3)]);

    if at(0) != PCP_VERSION {
        return Err(Rejected::WrongVersion {
            want: PCP_VERSION,
            got: at(0),
        });
    }
    if at(1) != (PCP_OP_MAP | PCP_RESPONSE) {
        return Err(Rejected::WrongOpcode {
            want: PCP_OP_MAP | PCP_RESPONSE,
            got: at(1),
        });
    }
    let code = u16::from(at(3));
    if code != 0 {
        return Err(Rejected::Refused {
            code,
            said: pcp_result(at(3)),
        });
    }
    let lifetime_secs = be32(4);

    // **The nonce is checked before anything is believed.** A reply naming a
    // different mapping is a reply about somebody else's port, and acting on
    // it would mean announcing an address that forwards somewhere else.
    if bytes.get(24..36) != Some(&nonce[..]) {
        return Err(Rejected::NotOurs);
    }
    if be16(40) != internal_port {
        return Err(Rejected::NotOurs);
    }
    let external_port = be16(42);
    let mut octets = [0u8; 16];
    for (slot, byte) in octets.iter_mut().zip(bytes.iter().skip(44)) {
        *slot = *byte;
    }
    let external_ip = std::net::Ipv6Addr::from(octets);
    // A router behind one NAT answers with an IPv4-mapped address; anything
    // else is an address this machine cannot advertise as a v4 endpoint.
    let Some(v4) = external_ip.to_ipv4_mapped() else {
        return Err(Rejected::NotOurs);
    };
    Ok(Granted {
        external: SocketAddrV4::new(v4, external_port),
        lifetime_secs,
    })
}

/// What a PCP result code means, in the RFC's own words, shortened.
const fn pcp_result(code: u8) -> &'static str {
    match code {
        1 => "it does not speak this version of PCP",
        2 => "this client is not authorised to ask",
        3 => "the request was malformed",
        4 => "it has no resources to spare",
        5 => "it does not support that protocol",
        6 => "that external port is already taken",
        7 => "too many mappings from this machine",
        8 => "it is not configured to forward anything",
        9 => "the address asked for is not this machine's",
        _ => "an error it did not name",
    }
}

/// A NAT-PMP mapping request. Twelve bytes, RFC 6886 §3.3.
#[must_use]
pub fn natpmp_request(internal_port: u16, want_external: u16, lifetime_secs: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.push(NATPMP_VERSION);
    out.push(NATPMP_OP_UDP);
    out.extend_from_slice(&[0, 0]); // reserved
    out.extend_from_slice(&internal_port.to_be_bytes());
    out.extend_from_slice(&want_external.to_be_bytes());
    out.extend_from_slice(&lifetime_secs.to_be_bytes());
    out
}

/// A NAT-PMP request for the router's public address. Two bytes, §3.2.
#[must_use]
pub fn natpmp_address_request() -> Vec<u8> {
    vec![NATPMP_VERSION, 0]
}

/// Read a NAT-PMP mapping response. Sixteen bytes, §3.3.
///
/// **The external address is not in it**, which is the protocol's real
/// limitation: a second request is needed for that, and a router can answer
/// the two inconsistently. The caller asks for the address separately.
///
/// # Errors
///
/// [`Rejected`] when the reply is malformed, is for another port, or says no.
pub fn natpmp_response(bytes: &[u8], internal_port: u16) -> Result<(u16, u32), Rejected> {
    if bytes.len() < 16 {
        return Err(Rejected::TooShort {
            what: "NAT-PMP",
            got: bytes.len(),
        });
    }
    let at = |i: usize| bytes.get(i).copied().unwrap_or(0);
    let be16 = |i: usize| u16::from_be_bytes([at(i), at(i + 1)]);
    let be32 = |i: usize| u32::from_be_bytes([at(i), at(i + 1), at(i + 2), at(i + 3)]);

    if at(0) != NATPMP_VERSION {
        return Err(Rejected::WrongVersion {
            want: NATPMP_VERSION,
            got: at(0),
        });
    }
    if at(1) != NATPMP_OP_UDP | 0x80 {
        return Err(Rejected::WrongOpcode {
            want: NATPMP_OP_UDP | 0x80,
            got: at(1),
        });
    }
    let code = be16(2);
    if code != 0 {
        return Err(Rejected::Refused {
            code,
            said: natpmp_result(code),
        });
    }
    if be16(8) != internal_port {
        return Err(Rejected::NotOurs);
    }
    Ok((be16(10), be32(12)))
}

/// Read a NAT-PMP address response. Twelve bytes, §3.2.
///
/// # Errors
///
/// [`Rejected`] when the reply is malformed or says no.
pub fn natpmp_address_response(bytes: &[u8]) -> Result<Ipv4Addr, Rejected> {
    if bytes.len() < 12 {
        return Err(Rejected::TooShort {
            what: "NAT-PMP address",
            got: bytes.len(),
        });
    }
    let at = |i: usize| bytes.get(i).copied().unwrap_or(0);
    if at(0) != NATPMP_VERSION {
        return Err(Rejected::WrongVersion {
            want: NATPMP_VERSION,
            got: at(0),
        });
    }
    if at(1) != 0x80 {
        return Err(Rejected::WrongOpcode {
            want: 0x80,
            got: at(1),
        });
    }
    let code = u16::from_be_bytes([at(2), at(3)]);
    if code != 0 {
        return Err(Rejected::Refused {
            code,
            said: natpmp_result(code),
        });
    }
    Ok(Ipv4Addr::new(at(8), at(9), at(10), at(11)))
}

/// What a NAT-PMP result code means.
const fn natpmp_result(code: u16) -> &'static str {
    match code {
        1 => "it does not speak this version of NAT-PMP",
        2 => "this client is not authorised to ask",
        3 => "it could not reach the network outside",
        4 => "it has no resources to spare",
        5 => "it does not support that operation",
        _ => "an error it did not name",
    }
}
