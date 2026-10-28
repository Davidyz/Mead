pub mod merkle;
pub mod query;
pub mod results;

pub use merkle::{Hash, MerkleProof, MerkleTree, SiblingDirection};
pub use query::{
    MAX_RESULTS_PER_PEER, QueryParams, QueryRegistry, ResultsPerPeer, SignedQuery,
    compute_query_id, query_signing_payload,
};
pub use results::{
    Bm25Score, MAX_MATCHES, ResultRegistry, SignedResult, SkillMatch, result_signing_payload,
};

use lyquid::prelude::{Address, LyquidError};

/// A trait for types that have a canonical byte representation for signing.
///
/// Implementors define their signing domain and canonical bytes; the signing
/// payload is then derived generically as `SIGNING_DOMAIN || canonical_bytes()`.
/// This lets signing/verification logic be written once and reused across
/// all signed structs.
pub trait CanonicalBytes {
    /// The domain separator prefix for this type's signing payload.
    const SIGNING_DOMAIN: &'static [u8];

    /// The canonical bytes of this struct, excluding the domain separator.
    fn canonical_bytes(&self) -> Vec<u8>;

    /// The full signing payload: domain separator followed by canonical bytes.
    fn signing_payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(Self::SIGNING_DOMAIN);
        payload.extend_from_slice(&self.canonical_bytes());
        payload
    }
}

/// A trait for types that carry a signature and a sender address.
///
/// Builds on [`CanonicalBytes`] to expose the fields needed for verification,
/// allowing signing/verification logic to be written once generically.
pub trait Signed: CanonicalBytes {
    /// The address of the entity that signed this message.
    fn sender(&self) -> Address;

    /// The raw signature bytes.
    fn signature(&self) -> &[u8];

}

pub(crate) fn invalid_input(message: impl Into<String>) -> LyquidError {
    LyquidError::LyquidRuntime(message.into())
}
