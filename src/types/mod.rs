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

use lyquid::prelude::LyquidError;

pub(crate) fn invalid_input(message: impl Into<String>) -> LyquidError {
    LyquidError::LyquidRuntime(message.into())
}
