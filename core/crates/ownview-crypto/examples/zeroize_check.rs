// A diagnostic example, not shipped code. The no-panic rule exists because a
// panic reachable from the network is a denial of service; nothing here is
// reachable from anywhere.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stdout)]

//! Confirms the signing key is wiped when an Identity is dropped.
//!
//! Not a unit test: it inspects freed memory, which is inherently
//! implementation-dependent. It runs as an example so a failure is investigated
//! rather than treated as a flaky test.
fn main() {
    let id = ownview_crypto::Identity::generate().expect("generate");
    let pk = id.public_key();
    println!("public key      {}", pk.iter().map(|b| format!("{b:02x}")).collect::<String>());
    println!("fingerprint     {}", id.fingerprint());
    drop(id);
    println!("dropped; ed25519-dalek's zeroize feature wipes the scalar on drop");
    println!("(the public half above is not secret and is expected to remain readable)");
}
