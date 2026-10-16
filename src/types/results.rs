//! On-chain search result data structures.
//!
//! When a peer node `Xᵢ` receives a search request, it runs a local search and
//! submits a `SignedResult` to the chain. The result includes a Merkle tree root
//! over the matched skills for efficient verification.

use lyquid::prelude::*;

use crate::types::merkle::{deserialize_hash, serialize_hash, Hash, MerkleTree};

/// A single matched skill with its BM25 relevance score.
///
/// The BM25 score is computed using the top-k skills as the entire corpus,
/// so that peers can independently verify the score.
///
/// Note: `Hash` (blake3::Hash) doesn't implement serde::Serialize/Deserialize
/// in the current lyquor_primitives setup (blake3 is built with default-features = false),
/// so we use custom serialization functions that convert to/from hex strings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillMatch {
    /// Digest of the matched skill.
    #[serde(serialize_with = "serialize_hash", deserialize_with = "deserialize_hash")]
    pub skill_digest: Hash,
    /// BM25 relevance score computed with the top-k skills as the corpus.
    pub bm25_score: f64,
}

/// A signed search result submitted on-chain by a peer node `Xᵢ`.
///
/// The result contains a list of matched skills with BM25 scores, a Merkle
/// tree root for efficient verification, and a signature by the submitting
/// peer node. The `query_digest` field binds this result to its originating
/// `SignedQuery`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedResult {
    /// Digest of the originating query text.
    #[serde(serialize_with = "serialize_hash", deserialize_with = "deserialize_hash")]
    pub query_digest: Hash,
    /// List of matched skills with their BM25 scores.
    pub matches: Vec<SkillMatch>,
    /// Merkle tree root hash over the list of matches
    /// (both skill digests and BM25 scores).
    #[serde(serialize_with = "serialize_hash", deserialize_with = "deserialize_hash")]
    pub merkle_root: Hash,
    /// UTC timestamp of when the result was submitted.
    pub timestamp: u64,
    /// Address of the submitting peer node `Xᵢ`.
    pub sender: Address,
    /// Signature of the result (over the matches and merkle root) by `Xᵢ`.
    pub signature: Vec<u8>,
}

impl SignedResult {
    /// Creates a new `SignedResult` with a Merkle tree committed to all matches.
    ///
    /// Each leaf is `blake3(skill_digest || bm25_score_bytes)` to ensure both
    /// the skill digest and its BM25 score are committed to the tree.
    pub fn new(query_digest: Hash, matches: Vec<SkillMatch>, sender: Address, signature: Vec<u8>) -> Self {
        let leaves: Vec<Hash> = matches
            .iter()
            .map(|m| {
                let mut data = [0u8; 40]; // 32 bytes digest + 8 bytes f64
                data[..32].copy_from_slice(m.skill_digest.as_bytes());
                data[32..].copy_from_slice(&m.bm25_score.to_le_bytes());
                lyquid::prelude::blake3::hash(&data)
            })
            .collect();
        let tree = MerkleTree::new(leaves);

        Self {
            query_digest,
            matches,
            merkle_root: tree.root(),
            timestamp: 0, // Caller should set this
            sender,
            signature,
        }
    }
}
