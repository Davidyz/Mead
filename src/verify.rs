//! Host-backed signature verification.
//!
//! The crypto backend lives here rather than in `types/` so the on-chain
//! data structures stay independent of it.

use lyquid::prelude::lyquor_api;
use lyquid::prelude::lyquor_primitives::{Bytes, Cipher, NodeID};

use crate::types::Signed;

/// Verifies any signed item implementing [`Signed`] + [`CanonicalBytes`].
///
/// Search identities are node identities, so the Ed25519 public key is carried
/// directly by the sender. No address-to-key lookup is involved.
pub fn verify_signed<T: Signed>(item: &T) -> bool {
    let Some(signature) = item.signature() else {
        return false;
    };

    verify_ed25519(item.signing_payload().as_slice(), signature, item.sender())
}

/// Verifies an Ed25519 signature over `payload` by the owner of `signer`.
///
/// The node ID's first 32 bytes are the Ed25519 public key. Any host-side
/// failure yields `false`: a verification that cannot be completed must never
/// be read as a valid signature.
pub fn verify_ed25519(payload: &[u8], signature: &[u8], signer: NodeID) -> bool {
    lyquor_api::verify(
        Bytes::copy_from_slice(payload),
        Cipher::Ed25519,
        Bytes::copy_from_slice(signature),
        Bytes::copy_from_slice(&signer.0),
    )
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[lyquid_test::test]
    fn rejects_a_signature_for_an_unrelated_node_id() {
        let signer = NodeID::new([0u8; 32]);
        assert!(!verify_ed25519(b"payload", &[0u8; 64], signer));
    }

    #[lyquid_test::test]
    fn rejects_a_different_node_id_than_the_signer() {
        let signer = NodeID::new([1u8; 32]);
        let other = NodeID::new([2u8; 32]);
        assert_eq!(
            verify_ed25519(b"payload", &[0u8; 64], signer),
            verify_ed25519(b"payload", &[0u8; 64], other)
        );
        assert!(!verify_ed25519(b"payload", &[0u8; 64], signer));
    }
}
