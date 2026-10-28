//! On-chain search result data structures.
//!
//! When a peer node `X岬?receives a search request, it runs a local search and
//! submits a `SignedResult` to the chain. The result includes a Merkle tree root
//! over the matched skills for efficient verification.

use core::cmp::Ordering;

use lyquid::prelude::*;

use crate::types::invalid_input;
use crate::types::merkle::{Hash, MerkleTree, deserialize_hash, serialize_hash};
use crate::types::query::{MAX_RESULTS_PER_PEER, SignedQuery};

const RESULT_SIGNING_DOMAIN: &[u8] = b"mead/result/v1";
const MATCH_LEAF_DOMAIN: &[u8] = b"mead/skill-match/v1";

/// Maximum number of matches accepted in a single result.
pub const MAX_MATCHES: usize = MAX_RESULTS_PER_PEER as usize;

/// Canonical integer representation of a BM25 score.
///
/// The fixed-point scale is defined by the scoring implementation and must be
/// identical across all peers and verifiers. Scores are always non-negative.
pub type Bm25Score = i64;

/// Registry that stores all submitted search results.
///
/// Maps `(query_id, submitter_address)` to the full `SignedResult`, ensuring
/// each peer can only submit one result per query.
#[derive(Debug, Clone)]
pub struct ResultRegistry {
    results: HashMap<(Hash, Address), SignedResult>,
}

impl Default for ResultRegistry {
    fn default() -> Self {
        Self {
            results: new_hashmap(),
        }
    }
}

impl ResultRegistry {
    /// Inserts a result after validating it against its query.
    ///
    /// Returns an error if the peer already submitted a result for this query.
    pub fn insert(&mut self, query: &SignedQuery, result: SignedResult) -> LyquidResult<()> {
        result.validate_against(query)?;

        let key = (result.query_id, result.sender);
        if self.results.contains_key(&key) {
            return Err(invalid_input(
                "a result for this query has already been submitted by this peer",
            ));
        }

        self.results.insert(key, result);
        Ok(())
    }

    /// Returns a result for the given query and peer.
    pub fn get(&self, query_id: &Hash, sender: Address) -> Option<&SignedResult> {
        self.results.get(&(*query_id, sender))
    }

    /// Returns true if the peer already submitted a result for this query.
    pub fn contains(&self, query_id: &Hash, sender: Address) -> bool {
        self.results.contains_key(&(*query_id, sender))
    }

    /// Returns the number of stored results.
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Returns true if no results are stored.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

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
    #[serde(
        serialize_with = "serialize_hash",
        deserialize_with = "deserialize_hash"
    )]
    pub skill_digest: Hash,
    /// BM25 relevance score computed with the top-k skills as the corpus,
    /// in the protocol's canonical fixed-point representation.
    pub bm25_score: Bm25Score,
}

impl SkillMatch {
    /// Validates the match fields for consensus and Merkle construction.
    pub fn validate(&self) -> LyquidResult<()> {
        if self.bm25_score < 0 {
            return Err(invalid_input("BM25 score must be non-negative"));
        }
        Ok(())
    }

    /// Canonical ordering used before building the Merkle tree.
    fn canonical_cmp(&self, other: &Self) -> Ordering {
        self.skill_digest
            .as_bytes()
            .cmp(other.skill_digest.as_bytes())
            .then_with(|| self.bm25_score.cmp(&other.bm25_score))
    }
}

/// A signed search result submitted on-chain by a peer node `X岬?
///
/// The result contains a list of matched skills with BM25 scores, a Merkle
/// tree root for efficient verification, and a signature by the submitting
/// peer node. The `query_id` field binds this result to its originating
/// `SignedQuery`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedResult {
    /// Canonical ID of the originating query.
    #[serde(
        serialize_with = "serialize_hash",
        deserialize_with = "deserialize_hash"
    )]
    pub query_id: Hash,
    /// List of matched skills with their BM25 scores, in canonical order.
    pub matches: Vec<SkillMatch>,
    /// Merkle tree root hash over the list of matches
    /// (both skill digests and BM25 scores).
    #[serde(
        serialize_with = "serialize_hash",
        deserialize_with = "deserialize_hash"
    )]
    pub merkle_root: Hash,
    /// UTC timestamp of when the result was submitted, in seconds since the Unix epoch.
    pub timestamp: u64,
    /// Address of the submitting peer node `X岬?
    pub sender: Address,
    /// Signature over [`SignedResult::signing_payload`] by `X岬?
    pub signature: Vec<u8>,
}

/// Returns the canonical bytes that a result submitter signs.
pub fn result_signing_payload(
    query_id: Hash,
    merkle_root: Hash,
    match_count: usize,
    timestamp: u64,
    sender: Address,
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(RESULT_SIGNING_DOMAIN);
    payload.extend_from_slice(query_id.as_bytes());
    payload.extend_from_slice(merkle_root.as_bytes());
    payload.extend_from_slice(&(match_count as u64).to_le_bytes());
    payload.extend_from_slice(&timestamp.to_le_bytes());
    payload.extend_from_slice(sender.as_slice());
    payload
}

fn canonicalize_matches(matches: &mut [SkillMatch]) -> LyquidResult<()> {
    if matches.len() > MAX_MATCHES {
        return Err(invalid_input("result contains too many matches"));
    }

    let mut seen = new_hashset();
    for skill_match in matches.iter() {
        skill_match.validate()?;
        if !seen.insert(skill_match.skill_digest) {
            return Err(invalid_input("result contains duplicate skill digests"));
        }
    }

    matches.sort_by(SkillMatch::canonical_cmp);
    Ok(())
}

fn matches_are_canonical(matches: &[SkillMatch]) -> bool {
    matches
        .windows(2)
        .all(|pair| pair[0].canonical_cmp(&pair[1]) == Ordering::Less)
}

impl MerkleTree {
    /// Creates a new Merkle tree from a list of skill matches.
    ///
    /// Matches are sorted into canonical order first. Each leaf commits to a
    /// domain-separated `skill_digest || bm25_score_bytes` payload, and the tree
    /// itself adds its own leaf/internal domain separation.
    pub fn from_matches(matches: &[SkillMatch]) -> Self {
        let mut ordered: Vec<&SkillMatch> = matches.iter().collect();
        ordered.sort_by(|left, right| left.canonical_cmp(right));

        let leaves: Vec<Hash> = ordered
            .iter()
            .map(|skill_match| {
                let mut hasher = blake3::Hasher::new();
                hasher.update(MATCH_LEAF_DOMAIN);
                hasher.update(skill_match.skill_digest.as_bytes());
                hasher.update(&skill_match.bm25_score.to_le_bytes());
                hasher.finalize()
            })
            .collect();

        Self::new(leaves)
    }
}

impl crate::types::CanonicalBytes for SignedResult {
    const SIGNING_DOMAIN: &'static [u8] = b"mead/result/v1";

    fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.query_id.as_bytes());
        bytes.extend_from_slice(self.merkle_root.as_bytes());
        bytes.extend_from_slice(&(self.matches.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(self.sender.as_slice());
        bytes
    }
}

impl crate::types::Signed for SignedResult {
    fn sender(&self) -> Address {
        self.sender
    }

    fn signature(&self) -> &[u8] {
        &self.signature
    }
}

impl SignedResult {
    /// Creates a new `SignedResult` with a Merkle tree committed to all matches.
    ///
    /// Matches are sorted into canonical order. `timestamp` must be supplied
    /// before signing; `signature` must cover [`SignedResult::signing_payload`]
    /// for the resulting fields.
    pub fn new(
        query_id: Hash,
        mut matches: Vec<SkillMatch>,
        timestamp: u64,
        sender: Address,
        signature: Vec<u8>,
    ) -> LyquidResult<Self> {
        canonicalize_matches(&mut matches)?;

        let tree = MerkleTree::from_matches(&matches);
        let result = Self {
            query_id,
            matches,
            merkle_root: tree.root(),
            timestamp,
            sender,
            signature,
        };
        result.validate()?;
        Ok(result)
    }

    /// Computes the signing payload for a result before it is constructed.
    ///
    /// This canonicalizes `matches` in place, so the caller can sign the payload
    /// and then pass the same ordering to [`SignedResult::new`].
    pub fn signing_payload_for(
        query_id: Hash,
        matches: &mut [SkillMatch],
        timestamp: u64,
        sender: Address,
    ) -> LyquidResult<Vec<u8>> {
        canonicalize_matches(matches)?;
        let tree = MerkleTree::from_matches(matches);
        Ok(result_signing_payload(
            query_id,
            tree.root(),
            matches.len(),
            timestamp,
            sender,
        ))
    }

    /// Validates the result independently of its query.
    pub fn validate(&self) -> LyquidResult<()> {
        if self.timestamp == 0 {
            return Err(invalid_input("result timestamp must be non-zero"));
        }
        if self.signature.is_empty() {
            return Err(invalid_input("result signature must not be empty"));
        }
        if self.matches.len() > MAX_MATCHES {
            return Err(invalid_input("result contains too many matches"));
        }

        let mut seen = new_hashset();
        for skill_match in &self.matches {
            skill_match.validate()?;
            if !seen.insert(skill_match.skill_digest) {
                return Err(invalid_input("result contains duplicate skill digests"));
            }
        }

        if !matches_are_canonical(&self.matches) {
            return Err(invalid_input("result matches are not in canonical order"));
        }

        let expected_root = MerkleTree::from_matches(&self.matches).root();
        if self.merkle_root != expected_root {
            return Err(invalid_input(
                "result Merkle root does not match the committed matches",
            ));
        }

        Ok(())
    }

    /// Validates this result against its originating query.
    pub fn validate_against(&self, query: &SignedQuery) -> LyquidResult<()> {
        query.validate()?;
        self.validate()?;

        if self.query_id != query.query_id {
            return Err(invalid_input("result does not belong to this query"));
        }
        if self.matches.len() as u64 > query.params.k {
            return Err(invalid_input("result contains more matches than requested"));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::query::QueryParams;

    fn skill(byte: u8, score: Bm25Score) -> SkillMatch {
        SkillMatch {
            skill_digest: blake3::hash(&[byte]),
            bm25_score: score,
        }
    }

    fn query_with_k(k: u64) -> SignedQuery {
        SignedQuery::new(b"query", QueryParams { k }, 1, Address::ZERO, vec![1]).unwrap()
    }

    #[lyquid_test::test]
    fn accepts_empty_match_set() {
        let query = query_with_k(10);
        let result =
            SignedResult::new(query.query_id, Vec::new(), 1, Address::ZERO, vec![1]).unwrap();

        assert!(result.matches.is_empty());
        assert!(result.validate_against(&query).is_ok());
    }

    #[lyquid_test::test]
    fn rejects_too_many_matches_for_query() {
        let query = query_with_k(1);
        let result = SignedResult::new(
            query.query_id,
            vec![skill(1, 1), skill(2, 0)],
            1,
            Address::ZERO,
            vec![1],
        )
        .unwrap();

        let mut registry = ResultRegistry::default();
        assert!(registry.insert(&query, result).is_err());
    }

    #[lyquid_test::test]
    fn rejects_duplicate_skill_digests() {
        let query = query_with_k(10);
        let result = SignedResult::new(
            query.query_id,
            vec![skill(1, 1), skill(1, 0)],
            1,
            Address::ZERO,
            vec![1],
        );

        assert!(result.is_err());
    }

    #[lyquid_test::test]
    fn rejects_negative_scores() {
        let query = query_with_k(10);
        let result = SignedResult::new(
            query.query_id,
            vec![skill(1, -1)],
            1,
            Address::ZERO,
            vec![1],
        );

        assert!(result.is_err());
    }

    #[lyquid_test::test]
    fn accepts_zero_score() {
        let query = query_with_k(10);
        let result =
            SignedResult::new(query.query_id, vec![skill(1, 0)], 1, Address::ZERO, vec![1])
                .unwrap();

        assert!(result.validate_against(&query).is_ok());
    }
}
