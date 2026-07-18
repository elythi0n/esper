#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Checking the bytes against something that has never seen this code.
//!
//! # Why an outside implementation
//!
//! A wire format tested only against its own reader is tested against its own
//! misunderstandings: encode a field in the wrong order, decode it in the same
//! wrong order, and every test passes while nothing else on the segment can
//! read a word of it.
//!
//! `dnspython` is an independent implementation of the same documents. What it
//! parses from our bytes has to be what we meant, and what it produces has to
//! be something we can read — including the name compression this deliberately
//! never writes, so the reader is exercised against pointers it would not
//! produce itself.
//!
//! # One thing it cannot check
//!
//! Multicast DNS overloads the top bit of the class field to mean "replace
//! what you had cached". To a *plain* DNS parser the class of such a record is
//! simply unknown, and its data comes back as uninterpreted bytes. So the
//! record contents are checked here with that bit cleared, and the bit itself
//! is checked against the bytes — it is an mDNS claim, and `dnspython` is not
//! an mDNS implementation.
//!
//! These skip when it is not installed, and say so: a check that quietly does
//! not run is worse than no check.

use std::process::Command;

use esper_discovery::name::Name;
use esper_discovery::responder::records_of;
use esper_discovery::service::{Advertisement, SERVICE};
use esper_discovery::wire::{Data, Message, Record, TYPE_PTR};

/// `Some(python)` when an outside DNS implementation is available.
fn python() -> Option<&'static str> {
    let ok = Command::new("python3")
        .args(["-c", "import dns.message"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        println!("skipped: python3 with dnspython is not installed; CI installs it");
        return None;
    }
    Some("python3")
}

/// Run a snippet with our packet available as `m`, already parsed.
fn with_packet(python: &str, packet: &[u8], code: &str) -> String {
    let hex: String = packet.iter().map(|b| format!("{b:02x}")).collect();
    let program = format!(
        "import sys, dns.message, dns.flags, dns.rdatatype\n\
         wire = bytes.fromhex('{hex}')\n\
         m = dns.message.from_wire(wire, question_only=False, ignore_trailing=True)\n\
         {code}\n"
    );
    let out = Command::new(python)
        .args(["-c", &program])
        .output()
        .expect("python ran");
    assert!(
        out.status.success(),
        "python could not read our packet:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn advertisement() -> Advertisement {
    Advertisement {
        device: "elythion".to_owned(),
        fingerprint: "c6a8baa216f1078e".to_owned(),
        port: 7645,
        protocol: 1,
        address: Some(std::net::Ipv4Addr::new(192, 168, 1, 20)),
        host: "elythion".to_owned(),
    }
}

fn response_of(advertisement: &Advertisement) -> Message {
    let records = records_of(advertisement).expect("records");
    let (answers, additional): (Vec<Record>, Vec<Record>) = records
        .into_iter()
        .partition(|r| matches!(r.data, Data::Ptr(_)));
    Message {
        id: 0,
        response: true,
        questions: Vec::new(),
        answers,
        additional,
    }
}

/// Everything an announcement says, read by somebody else's parser.
#[test]
fn an_announcement_reads_the_same_to_an_outside_implementation() {
    let Some(python) = python() else { return };
    let mut message = response_of(&advertisement());
    for r in message
        .answers
        .iter_mut()
        .chain(message.additional.iter_mut())
    {
        r.cache_flush = false;
    }

    let seen = with_packet(
        python,
        &message.encode(),
        "print('flags', dns.flags.to_text(m.flags))\n\
         for rrset in m.answer + m.additional:\n\
         \x20   for rd in rrset:\n\
         \x20       print(rrset.name.to_text(), dns.rdatatype.to_text(rd.rdtype), rd.to_text())",
    );
    println!("{seen}");

    assert!(
        seen.contains("_esper._udp.local. PTR"),
        "the pointer must be an answer for the service type:\n{seen}"
    );
    // Escaped as the documents escape it — a space is `\032`, and `dnspython`
    // escapes the brackets too. What matters is that it survives as *one*
    // label rather than being split at the space or truncated.
    assert!(
        seen.contains(r"elythion\032\(c6a8baa2\)._esper._udp.local."),
        "the instance name must survive, space and brackets and all:\n{seen}"
    );
    assert!(
        seen.contains("SRV 0 0 7645 elythion.local."),
        "the port and target must be where a resolver looks for them:\n{seen}"
    );
    assert!(
        seen.contains(r#"TXT "fp=c6a8baa216f1078e" "pv=1""#),
        "both text keys must be present and separately readable:\n{seen}"
    );
    assert!(
        seen.contains("A 192.168.1.20"),
        "the address record must be readable:\n{seen}"
    );
    assert!(
        seen.contains("QR") && seen.contains("AA"),
        "the header must say response and authoritative:\n{seen}"
    );
}

/// **The bit a plain DNS parser cannot see.**
///
/// It belongs on the records unique to this device — its port, its text, its
/// address — and emphatically not on the pointer, which every Esper on the
/// segment shares: flushing that would tell a receiver to forget all the
/// others.
#[test]
fn the_cache_flush_bit_is_set_on_the_unique_records_and_not_the_shared_one() {
    let records = records_of(&advertisement()).expect("records");
    assert!(records.len() >= 4, "there should be a PTR, SRV, TXT and A");
    for r in &records {
        let shared = matches!(r.data, Data::Ptr(_));
        assert_eq!(
            r.cache_flush, !shared,
            "{:?} has the wrong cache-flush setting",
            r.data
        );
    }

    // And in the bytes: the class field of a flushed record has its top bit
    // set, which is exactly what our own reader reports back.
    let packet = response_of(&advertisement()).encode();
    let read = Message::decode(&packet).expect("our own packet");
    assert!(
        read.answers.iter().all(|r| !r.cache_flush),
        "the shared pointer must not be flushed"
    );
    assert!(
        read.additional.iter().all(|r| r.cache_flush),
        "every unique record must be"
    );
}

/// A query, read by somebody else's parser.
#[test]
fn a_query_reads_the_same_to_an_outside_implementation() {
    let Some(python) = python() else { return };
    let packet = Message::query(Name::parse(SERVICE).expect("a name"), TYPE_PTR).encode();
    let seen = with_packet(
        python,
        &packet,
        "q = m.question[0]\n\
         print(q.name.to_text(), dns.rdatatype.to_text(q.rdtype), 'QR' in dns.flags.to_text(m.flags))",
    );
    assert_eq!(seen, "_esper._udp.local. PTR False", "{seen}");
}

/// **The other direction, with name compression.**
///
/// `dnspython` compresses names when it writes, so this is the reader tested
/// against pointers it never produces itself — including a pointer into a name
/// that is itself only reachable through the pointer before it.
#[test]
fn a_response_built_elsewhere_is_read_correctly() {
    let Some(python) = python() else { return };
    // Built from label bytes rather than text, so the instance name with its
    // space and brackets goes on the wire exactly as intended without a round
    // of escaping rules in the way.
    let program = "\
import sys, dns.message, dns.name, dns.rrset, dns.rdataclass, dns.rdatatype
import dns.rdtypes.ANY.PTR, dns.rdtypes.ANY.TXT, dns.rdtypes.IN.SRV, dns.rdtypes.IN.A
IN = dns.rdataclass.IN
service = dns.name.Name([b'_esper', b'_udp', b'local', b''])
instance = dns.name.Name([b'other (aabbccdd)', b'_esper', b'_udp', b'local', b''])
host = dns.name.Name([b'other', b'local', b''])
m = dns.message.Message()
m.flags |= dns.flags.QR | dns.flags.AA

ptr = dns.rrset.RRset(service, IN, dns.rdatatype.PTR)
ptr.add(dns.rdtypes.ANY.PTR.PTR(IN, dns.rdatatype.PTR, instance), ttl=4500)
m.answer.append(ptr)

srv = dns.rrset.RRset(instance, IN, dns.rdatatype.SRV)
srv.add(dns.rdtypes.IN.SRV.SRV(IN, dns.rdatatype.SRV, 0, 0, 9999, host), ttl=120)
m.additional.append(srv)

txt = dns.rrset.RRset(instance, IN, dns.rdatatype.TXT)
txt.add(dns.rdtypes.ANY.TXT.TXT(IN, dns.rdatatype.TXT, [b'fp=aabbccdd11223344', b'pv=1']), ttl=120)
m.additional.append(txt)

a = dns.rrset.RRset(host, IN, dns.rdatatype.A)
a.add(dns.rdtypes.IN.A.A(IN, dns.rdatatype.A, '10.1.2.3'), ttl=120)
m.additional.append(a)

sys.stdout.write(m.to_wire().hex())
";
    let out = Command::new(python)
        .args(["-c", program])
        .output()
        .expect("python ran");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hex = String::from_utf8_lossy(&out.stdout);
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect();
    assert!(!bytes.is_empty(), "python produced no packet");
    // It really did compress, or this test is not testing what it claims.
    assert!(
        bytes
            .windows(2)
            .any(|w| matches!(w, [b, _] if b & 0xC0 == 0xC0)),
        "the packet has no compression pointer in it, so the reader is not being \
         tested against one"
    );

    let message = Message::decode(&bytes).expect("our reader must read it");
    assert!(message.response);

    let pointed = message
        .answers
        .iter()
        .find_map(|r| match &r.data {
            Data::Ptr(name) => Some(name.to_string()),
            _ => None,
        })
        .expect("a pointer");
    assert!(
        pointed.starts_with("other (aabbccdd)"),
        "the instance name must come back whole through the pointers: {pointed}"
    );

    let port = message
        .additional
        .iter()
        .find_map(|r| match &r.data {
            Data::Srv { port, .. } => Some(*port),
            _ => None,
        })
        .expect("a service record");
    assert_eq!(port, 9999);

    let txt: Vec<String> = message
        .additional
        .iter()
        .filter_map(|r| match &r.data {
            Data::Txt(strings) => Some(
                strings
                    .iter()
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(txt, vec!["fp=aabbccdd11223344,pv=1".to_owned()]);

    let address = message.additional.iter().find_map(|r| match &r.data {
        Data::A(a) => Some(*a),
        _ => None,
    });
    assert_eq!(address, Some(std::net::Ipv4Addr::new(10, 1, 2, 3)));
}

/// A goodbye is a record set with nothing left to live, and an outside
/// implementation has to see it as that.
#[test]
fn a_goodbye_is_a_zero_ttl_to_an_outside_implementation() {
    let Some(python) = python() else { return };
    let mut message = response_of(&advertisement());
    for r in message
        .answers
        .iter_mut()
        .chain(message.additional.iter_mut())
    {
        r.ttl = 0;
        r.cache_flush = false;
    }
    let seen = with_packet(
        python,
        &message.encode(),
        "ttls = [r.ttl for r in m.answer + m.additional]\nprint(min(ttls), max(ttls), len(ttls))",
    );
    assert_eq!(seen, "0 0 4", "every record must have expired: {seen}");
}
