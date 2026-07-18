// A diagnostic example, not shipped code. The no-panic rule exists because a
// panic reachable from the network is a denial of service; nothing here is
// reachable from anywhere.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stdout)]

fn main() {
    for (label, key) in [
        ("all zeros", [0u8; 32]),
        ("all 0xAB", [0xABu8; 32]),
        ("0x01..", {
            let mut k = [0u8; 32];
            k[0] = 1;
            k
        }),
    ] {
        let f = esper_crypto::Fingerprint::of(&key);
        println!("{label}:\n{}\n  hex {}\n", f.display(), f.hex());
    }
}
