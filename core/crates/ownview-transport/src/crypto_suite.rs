//! The cryptographic configuration of every OwnView connection.
//!
//! There is one key exchange group and there is no list. The reasoning is
//! stated here in full rather than referenced, because this is the file someone
//! reads when they are about to add a second group.
//!
//! # Why one group
//!
//! rustls offers a *list* of key exchange groups with the post-quantum hybrid
//! first. That is correct for a public web server, which must serve every
//! client that exists. It is wrong here.
//!
//! A list expresses a **preference**, and a preference is satisfied by whatever
//! the other side agrees to. Measured against a peer offering only classical
//! X25519, a hybrid-preferred client connects on classical — successfully,
//! silently, with nothing logged. An active attacker who strips the hybrid from
//! the ClientHello produces exactly that outcome.
//!
//! Offering a single group converts downgrade from a risk that must be detected
//! into a state that cannot exist. The handshake either uses
//! `X25519MLKEM768` or it fails closed.
//!
//! This costs nothing, for a reason specific to this product: **both ends are
//! ours.** There is no browser, no legacy server, no long tail of old clients.
//! Refusing a classical-only peer refuses nothing anyone wants to reach.

use rustls::crypto::{CryptoProvider, SupportedKxGroup, aws_lc_rs};

/// The one key exchange group OwnView will negotiate.
///
/// Hybrid: an attacker must break **both** X25519 and ML-KEM-768. Adopting
/// ML-KEM does not weaken the classical guarantee, which is the entire reason
/// for going hybrid rather than pure post-quantum.
pub const KX_GROUP: &dyn SupportedKxGroup = aws_lc_rs::kx_group::X25519MLKEM768;

/// TLS 1.3 only. There is no reason for an earlier version to exist here and
/// several reasons for it not to.
pub const TLS_VERSIONS: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

/// Build the crypto provider every OwnView connection uses.
///
/// The returned provider offers exactly one key exchange group. If you are
/// tempted to add a second "for compatibility", read the module docs first:
/// that is adding a downgrade path.
#[must_use]
pub fn provider() -> CryptoProvider {
    let mut p = aws_lc_rs::default_provider();
    p.kx_groups = vec![KX_GROUP];
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property this module exists to guarantee. Configuration drifts; a
    /// failing test does not.
    #[test]
    fn exactly_one_key_exchange_group_is_offered() {
        let p = provider();
        assert_eq!(
            p.kx_groups.len(),
            1,
            "offering more than one group reintroduces a silent downgrade path; \
             read this module's documentation before changing it. \
             Extra groups: {:?}",
            p.kx_groups.iter().map(|g| g.name()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_one_group_is_the_post_quantum_hybrid() {
        let p = provider();
        let name = format!("{:?}", p.kx_groups.first().expect("one group").name());
        assert!(
            name.contains("MLKEM"),
            "the offered group is {name}, which is not post-quantum"
        );
        assert!(
            name.contains("X25519"),
            "the offered group is {name}, which is not hybrid — a pure PQ group \
             would lose the classical guarantee if ML-KEM were broken"
        );
    }

    /// Guards against a dependency change quietly removing the group. If
    /// rustls renames or drops `X25519MLKEM768`, this fails rather than the
    /// product silently negotiating something else.
    #[test]
    fn the_group_is_the_one_we_named() {
        assert_eq!(
            format!("{:?}", KX_GROUP.name()),
            "X25519MLKEM768",
            "the key exchange group changed; it is pinned deliberately"
        );
    }

    /// A classical group must not be reachable through our provider, even if
    /// something later iterates the provider looking for one.
    #[test]
    fn no_classical_group_is_reachable() {
        for g in provider().kx_groups {
            let name = format!("{:?}", g.name());
            assert!(
                name.contains("MLKEM"),
                "provider still offers the classical group {name}"
            );
        }
    }

    #[test]
    fn only_tls13_is_permitted() {
        assert_eq!(TLS_VERSIONS.len(), 1);
        assert_eq!(
            format!("{:?}", TLS_VERSIONS.first().expect("one version").version),
            "TLSv1_3"
        );
    }
}
