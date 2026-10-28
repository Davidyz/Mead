//! Host-backed signature verification and caller authorization.
//!
//! The crypto backend lives here rather than in `types/` so the on-chain
//! data structures stay independent of it.

use lyquid::prelude::LyquidResult;
use lyquid::prelude::lyquor_api;
use lyquid::prelude::lyquor_primitives::{Address, Bytes, Cipher};

use crate::types::invalid_input;
use crate::types::{CanonicalBytes, Signed, SignedQuery, SignedResult};

/// Verifies any signed item implementing [`Signed`] + [`CanonicalBytes`].
///
/// Single verification entry point: looks up the sender's Ed25519 public key
/// and verifies the signature over the canonical signing payload.
pub fn verify_signed<T: Signed>(item: &T) -> bool {
    verify_ed25519(
        item.signing_payload().as_slice(),
        item.signature(),
        item.sender(),
    )
}

/// Returns the Ed25519 public key bound to `signer`, if that node is registered.
fn verifying_key(signer: Address) -> Option<[u8; 32]> {
    lyquor_api::get_ed25519_by_address(signer)
        .ok()
        .flatten()
        .map(|node_id| node_id.0)
}

/// Verifies an Ed25519 signature over `payload` by the owner of `signer`.
///
/// An unknown signer, or any host-side failure, yields `false`: a lookup that
/// cannot resolve a key must never be read as a valid signature.
pub fn verify_ed25519(payload: &[u8], signature: &[u8], signer: Address) -> bool {
    let Some(public_key) = verifying_key(signer) else {
        return false;
    };

    lyquor_api::verify(
        Bytes::copy_from_slice(payload),
        Cipher::Ed25519,
        Bytes::copy_from_slice(signature),
        Bytes::copy_from_slice(&public_key),
    )
    .unwrap_or(false)
}

/// Enforces who may submit a message on behalf of `sender`.
///
/// Mead has no delegation registry yet, so the only relationship accepted today
/// is self-submission: a node may register only messages it signed itself. This
/// is what turns a valid signature into an authorization decision, because a
/// signature alone only proves the query is authentic, not that the caller was
/// entitled to post it. A relayer or delegate model would extend this function
/// rather than the data structures.
pub fn ensure_authorized(caller: Address, sender: Address) -> LyquidResult<()> {
    if caller != sender {
        return Err(invalid_input(
            "caller is not authorized to submit for this sender",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn other_address() -> Address {
        Address::from([1u8; 20])
    }

    #[lyquid_test::test]
    fn accepts_self_submission() {
        assert!(ensure_authorized(Address::ZERO, Address::ZERO).is_ok());
        assert!(ensure_authorized(other_address(), other_address()).is_ok());
    }

    #[lyquid_test::test]
    fn rejects_third_party_submission() {
        assert!(ensure_authorized(other_address(), Address::ZERO).is_err());
        assert!(ensure_authorized(Address::ZERO, other_address()).is_err());
    }

    #[lyquid_test::test]
    fn rejects_signature_from_unknown_signer() {
        // An address with no registered Ed25519 key can never produce a valid
        // signature, so verification must fail closed rather than error out.
        assert!(!verify_ed25519(b"payload", b"signature", Address::ZERO));
        assert!(!verify_ed25519(b"payload", b"signature", other_address()));
    }
}
