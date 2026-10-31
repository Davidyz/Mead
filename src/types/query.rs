//! On-chain search query data structures.
//!
//! When a node `X₀` initiates a search, it submits a `SearchQuery` to the chain.
//! The query ID is stored on-chain so peers can verify they are responding to the
//! same search, including its parameters, timestamp, and sender.

use lyquid::prelude::*;

use crate::types::invalid_input;
use crate::types::merkle::{deserialize_hash, serialize_hash};

/// Maximum number of results a peer may be asked to return, and the maximum
/// number of matches accepted in a result.
pub const MAX_RESULTS_PER_PEER: u64 = 1024;

/// Number of sequencer blocks a query waits for its expected responders.
///
/// Once this many blocks have passed since registration, the query is treated
/// as complete with the results received so far, so one unresponsive peer
/// cannot keep the initiating node waiting forever.
pub const SEARCH_RESPONSE_DEADLINE_BLOCKS: u64 = 50;

const QUERY_ID_DOMAIN: &[u8] = b"mead/query-id/v1";

/// Registry that tracks all submitted queries and their responders.
///
/// - `queries`: maps query ID to the full SearchQuery
/// - `responder_index`: maps query ID to the set of addresses that submitted results
/// - `expected_nodes`: maps query ID to the peer nodes that should be notified
/// - `expected_responders`: maps query ID to the addresses that must respond
/// - `initiator_index`: maps query ID to the node that started it
/// - `deadline_index`: maps query ID to the chain position after which the
///   query stops waiting for missing responders
#[derive(Debug, Clone)]
pub struct QueryRegistry {
    queries: HashMap<Hash, SearchQuery>,
    responder_index: HashMap<Hash, HashSet<Address>>,
    expected_nodes: HashMap<Hash, Vec<NodeID>>,
    expected_responders: HashMap<Hash, HashSet<Address>>,
    initiator_index: HashMap<Hash, NodeID>,
    deadline_index: HashMap<Hash, ChainPos>,
}

impl Default for QueryRegistry {
    fn default() -> Self {
        Self {
            queries: new_hashmap(),
            responder_index: new_hashmap(),
            expected_nodes: new_hashmap(),
            expected_responders: new_hashmap(),
            initiator_index: new_hashmap(),
            deadline_index: new_hashmap(),
        }
    }
}

impl QueryRegistry {
    /// Registers a query, rejecting duplicate query IDs.
    pub fn insert(&mut self, query: SearchQuery) -> LyquidResult<()> {
        query.validate()?;

        if self.queries.contains_key(&query.query_id) {
            return Err(invalid_input("query is already registered"));
        }

        let query_id = query.query_id;
        self.queries.insert(query_id, query);
        self.responder_index.insert(query_id, new_hashset());
        Ok(())
    }

    /// Returns a query by ID.
    pub fn get(&self, query_id: &Hash) -> Option<&SearchQuery> {
        self.queries.get(query_id)
    }

    /// Returns true if the query ID is registered.
    pub fn contains(&self, query_id: &Hash) -> bool {
        self.queries.contains_key(query_id)
    }

    /// Records an address as having submitted a result for this query.
    pub fn mark_responder(&mut self, query_id: Hash, responder: Address) -> LyquidResult<()> {
        let responders = self
            .responder_index
            .get_mut(&query_id)
            .ok_or_else(|| invalid_input("cannot record a responder for an unknown query"))?;
        responders.insert(responder);
        Ok(())
    }

    /// Returns the responder set for a query, if the query exists.
    pub fn responders(&self, query_id: &Hash) -> Option<&HashSet<Address>> {
        self.responder_index.get(query_id)
    }

    /// Records the peers expected to process this query, the node that started
    /// it, and the position after which missing responders no longer block.
    ///
    /// The query must already be registered. `expected_nodes` is the notification
    /// target list handed to the UPC prepare step, `expected_responders` is the
    /// set of addresses that must submit a result before the query is complete,
    /// and `deadline` bounds how long that set is waited for.
    pub fn plan(
        &mut self,
        query_id: Hash,
        initiator: Option<NodeID>,
        expected_nodes: Vec<NodeID>,
        expected_responders: HashSet<Address>,
        deadline: ChainPos,
    ) -> LyquidResult<()> {
        if !self.queries.contains_key(&query_id) {
            return Err(invalid_input("cannot plan an unknown query"));
        }

        self.expected_nodes.insert(query_id, expected_nodes);
        self.expected_responders
            .insert(query_id, expected_responders);
        self.deadline_index.insert(query_id, deadline);
        if let Some(initiator) = initiator {
            self.initiator_index.insert(query_id, initiator);
        }
        Ok(())
    }

    /// Returns the peer nodes that should be notified about this query.
    pub fn expected_nodes(&self, query_id: &Hash) -> Option<&[NodeID]> {
        self.expected_nodes
            .get(query_id)
            .map(|nodes| nodes.as_slice())
    }

    /// Returns the node that initiated this query, if it was resolvable.
    pub fn initiator_node(&self, query_id: &Hash) -> Option<NodeID> {
        self.initiator_index.get(query_id).copied()
    }

    /// Returns the chain position after which the query stops waiting for
    /// missing responders.
    pub fn deadline(&self, query_id: &Hash) -> Option<ChainPos> {
        self.deadline_index.get(query_id).copied()
    }

    /// Returns true once every expected responder has submitted a result, or
    /// once the response deadline at `position` has passed.
    ///
    /// A query without a plan is never complete. A plan with no expected
    /// responders is complete by definition, since there is nothing to wait for.
    /// After the deadline, a query is complete even if some expected peers never
    /// answered, so their absence cannot block the initiating node forever.
    pub fn is_complete(&self, query_id: &Hash, position: ChainPos) -> bool {
        let Some(expected) = self.expected_responders.get(query_id) else {
            return false;
        };
        let Some(actual) = self.responder_index.get(query_id) else {
            return false;
        };

        if expected.iter().all(|address| actual.contains(address)) {
            return true;
        }

        self.deadline_index
            .get(query_id)
            .is_some_and(|deadline| *deadline <= position)
    }

    /// Returns the number of registered queries.
    pub fn len(&self) -> usize {
        self.queries.len()
    }

    /// Returns true if no queries are registered.
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
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

impl QueryParams {
    /// Validates the query parameters against the supported bounds.
    pub fn validate(&self) -> LyquidResult<()> {
        if self.k == 0 {
            return Err(invalid_input("query parameter k must be greater than zero"));
        }
        if self.k > MAX_RESULTS_PER_PEER {
            return Err(invalid_input("query parameter k exceeds the maximum"));
        }
        Ok(())
    }
}

/// Computes the canonical ID of a query from every signed field except the
/// signature itself.
///
/// Queries with the same text but different `k`, timestamp, or sender therefore
/// have different IDs and cannot overwrite each other in the registry.
pub fn compute_query_id(
    query_digest: Hash,
    params: &QueryParams,
    timestamp: u64,
    sender: Address,
) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(QUERY_ID_DOMAIN);
    hasher.update(query_digest.as_bytes());
    hasher.update(&params.k.to_le_bytes());
    hasher.update(&timestamp.to_le_bytes());
    hasher.update(sender.as_slice());
    hasher.finalize()
}

/// A search query submitted on-chain by the querying node `X₀`.
///
/// The query ID commits to the query text digest, parameters, timestamp, and
/// sender. Peers bind their results to this ID.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchQuery {
    /// Canonical ID of the full query.
    #[serde(
        serialize_with = "serialize_hash",
        deserialize_with = "deserialize_hash"
    )]
    pub query_id: Hash,
    /// Blake3 digest of the query text.
    #[serde(
        serialize_with = "serialize_hash",
        deserialize_with = "deserialize_hash"
    )]
    pub query_digest: Hash,
    /// Search parameters (e.g., number of results per peer).
    pub params: QueryParams,
    /// UTC timestamp of when the query was created, in seconds since the Unix epoch.
    pub timestamp: u64,
    /// Address of the querying node `X₀`.
    pub sender: Address,
    /// Signature over [`crate::types::CanonicalBytes::signing_payload`] by `X₀`, if present.
    signature: Option<Vec<u8>>,
}

impl crate::types::CanonicalBytes for SearchQuery {
    const SIGNING_DOMAIN: &'static [u8] = b"mead/query/v1";

    fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.query_digest.as_bytes());
        bytes.extend_from_slice(&self.params.k.to_le_bytes());
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(self.sender.as_slice());
        bytes
    }
}

impl crate::types::Signed for SearchQuery {
    fn sender(&self) -> Address {
        self.sender
    }

    fn signature(&self) -> Option<&[u8]> {
        self.signature.as_deref()
    }
}

impl SearchQuery {
    /// Creates a query from the query text, computing its digest and canonical ID.
    ///
    /// The returned query is unsigned; attach a signature with
    /// [`SearchQuery::set_signature`].
    pub fn new(
        query: impl AsRef<[u8]>,
        params: QueryParams,
        timestamp: u64,
        sender: Address,
    ) -> LyquidResult<Self> {
        let query_digest = blake3::hash(query.as_ref());
        Self::from_digest(query_digest, params, timestamp, sender)
    }

    /// Creates a query from an already-computed query-text digest.
    pub fn from_digest(
        query_digest: Hash,
        params: QueryParams,
        timestamp: u64,
        sender: Address,
    ) -> LyquidResult<Self> {
        let query_id = compute_query_id(query_digest, &params, timestamp, sender);
        let query = Self {
            query_id,
            query_digest,
            params,
            timestamp,
            sender,
            signature: None,
        };
        query.validate()?;
        Ok(query)
    }

    /// Attaches the signature over [`crate::types::CanonicalBytes::signing_payload`].
    pub fn set_signature(&mut self, signature: Vec<u8>) {
        self.signature = Some(signature);
    }

    /// Validates the query's fields and its derived query ID.
    pub fn validate(&self) -> LyquidResult<()> {
        self.params.validate()?;

        if self.timestamp == 0 {
            return Err(invalid_input("query timestamp must be non-zero"));
        }
        let expected_id =
            compute_query_id(self.query_digest, &self.params, self.timestamp, self.sender);
        if self.query_id != expected_id {
            return Err(invalid_input("query ID does not match the query fields"));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_query() -> SearchQuery {
        SearchQuery::new(b"rust async", QueryParams { k: 10 }, 1, Address::ZERO).unwrap()
    }

    #[lyquid_test::test]
    fn rejects_zero_k() {
        assert!(QueryParams { k: 0 }.validate().is_err());
    }

    #[lyquid_test::test]
    fn rejects_oversized_k() {
        assert!(
            QueryParams {
                k: MAX_RESULTS_PER_PEER + 1
            }
            .validate()
            .is_err()
        );
    }

    #[lyquid_test::test]
    fn signature_is_attached_after_construction() {
        let mut query = valid_query();
        assert!(query.signature.is_none());

        query.set_signature(vec![1]);
        assert_eq!(query.signature, Some(vec![1]));
    }

    #[lyquid_test::test]
    fn rejects_mismatched_query_id() {
        let mut query = valid_query();
        query.query_id = blake3::hash(b"different query");
        assert!(query.validate().is_err());
    }

    #[lyquid_test::test]
    fn rejects_duplicate_query_registration() {
        let query = valid_query();
        let mut registry = QueryRegistry::default();
        registry.insert(query.clone()).unwrap();
        assert!(registry.insert(query).is_err());
    }

    fn registered_query() -> (QueryRegistry, Hash, NodeID, Address) {
        let query = valid_query();
        let query_id = query.query_id;
        let initiator = NodeID::new([1u8; 32]);
        let peer = Address::from([2u8; 20]);

        let mut registry = QueryRegistry::default();
        registry.insert(query).unwrap();
        (registry, query_id, initiator, peer)
    }

    #[lyquid_test::test]
    fn rejects_plan_for_unknown_query() {
        let mut registry = QueryRegistry::default();
        let mut expected = new_hashset();
        expected.insert(Address::from([2u8; 20]));

        assert!(
            registry
                .plan(
                    blake3::hash(b"unknown"),
                    Some(NodeID::new([1u8; 32])),
                    Vec::new(),
                    expected,
                    ChainPos::ZERO,
                )
                .is_err()
        );
    }

    #[lyquid_test::test]
    fn complete_only_after_all_expected_responders_submit() {
        let (mut registry, query_id, initiator, peer) = registered_query();
        let mut expected = new_hashset();
        expected.insert(peer);
        let deadline = ChainPos::new(100, 0);

        registry
            .plan(
                query_id,
                Some(initiator),
                vec![initiator],
                expected,
                deadline,
            )
            .unwrap();

        assert_eq!(registry.initiator_node(&query_id), Some(initiator));
        assert_eq!(
            registry.expected_nodes(&query_id),
            Some([initiator].as_slice())
        );
        assert!(!registry.is_complete(&query_id, ChainPos::ZERO));

        registry.mark_responder(query_id, peer).unwrap();
        assert!(registry.is_complete(&query_id, ChainPos::ZERO));
    }

    #[lyquid_test::test]
    fn deadline_completes_query_with_missing_responder() {
        let (mut registry, query_id, initiator, peer) = registered_query();
        let mut expected = new_hashset();
        expected.insert(peer);
        let deadline = ChainPos::new(100, 0);

        registry
            .plan(
                query_id,
                Some(initiator),
                vec![initiator],
                expected,
                deadline,
            )
            .unwrap();

        assert_eq!(registry.deadline(&query_id), Some(deadline));
        assert!(!registry.is_complete(&query_id, ChainPos::new(99, 0)));
        assert!(registry.is_complete(&query_id, deadline));
        assert!(registry.is_complete(&query_id, ChainPos::new(101, 0)));
    }

    #[lyquid_test::test]
    fn unplanned_query_is_never_complete() {
        let (registry, query_id, _, _) = registered_query();
        assert!(!registry.is_complete(&query_id, ChainPos::new(10_000, 0)));
    }

    #[lyquid_test::test]
    fn empty_expected_set_is_complete() {
        let (mut registry, query_id, initiator, _) = registered_query();
        registry
            .plan(
                query_id,
                Some(initiator),
                Vec::new(),
                new_hashset(),
                ChainPos::new(100, 0),
            )
            .unwrap();

        assert!(registry.is_complete(&query_id, ChainPos::ZERO));
    }
}
