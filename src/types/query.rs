//! On-chain search query data structures.
//!
//! When a node `X₀` initiates a search, it submits a `SignedQuery` to the chain.
//! The query digest is stored on-chain so peers can verify they are responding
//! to the same search.

use lyquid::prelude::*;

use crate::types::merkle::{deserialize_hash, serialize_hash};

/// Registry that tracks all submitted queries and their responders.
///
/// - `queries`: maps query digest to the full SignedQuery
/// - `responder_index`: maps query digest to the set of addresses that submitted results
#[derive(Debug, Clone)]
pub struct QueryRegistry {
    /// Query digest -> SignedQuery.
    pub queries: HashMap<Hash, SignedQuery>,
    /// Query digest -> set of responder addresses.
    pub responder_index: HashMap<Hash, HashSet<Address>>,
}

impl Default for QueryRegistry {
    fn default() -> Self {
        Self {
            queries: new_hashmap(),
            responder_index: new_hashmap(),
        }
    }
}

/// The number of results each peer should return for a search query.
pub type ResultsPerPeer = u64;

/// Parameters for a search query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueryParams {
    /// Number of top results to return per peer node.
    pub k: ResultsPerPeer,
}

/// A signed search query submitted on-chain by the querying node `X₀`.
///
/// The query digest is stored on-chain so peers can verify they are responding
/// to the same search. Peers sign their results with this digest to bind
/// results to the originating query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignedQuery {
    /// Blake3 digest of the query text.
    #[serde(
        serialize_with = "serialize_hash",
        deserialize_with = "deserialize_hash"
    )]
    pub query_digest: Hash,
    /// Search parameters (e.g., number of results per peer).
    pub params: QueryParams,
    /// UTC timestamp of when the query was created.
    pub timestamp: u64,
    /// Address of the querying node `X₀`.
    pub sender: Address,
    /// Signature of the query (over the digest and params) by `X₀`.
    pub signature: Vec<u8>,
}
