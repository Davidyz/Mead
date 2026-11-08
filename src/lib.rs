//! Mead: blockchain-based P2P skill distribution platform.
//!
//! On-chain state consists of a skill directory and query/result registries
//! that track search queries and peer-submitted results.

use lyquid::prelude::lyquor_primitives::{Cipher, HashBytes, InputABI};
use lyquid::prelude::*;

pub mod skill;
pub mod types;
pub mod verify;

mod api;

use api::skills::{
    ensure_skills_loaded, ensure_skills_loaded_from, ensure_skills_loaded_immutable,
    handle_skill_upload, skill_count_response, skill_list_json,
};
use api::{
    PeerSearchReply, SearchReply, SearchReplyCache, parse_query_id_request,
    parse_search_endpoint_request, search_accepted_response, search_pending_response,
    search_results_response, search_status_response,
};
use api::{http_json_response, json_error};

use skill::{Skill, SkillError, SkillManager};
use types::invalid_input;
use types::{
    Bm25Score, CanonicalBytes, NodeRegistry, QueryParams, QueryRegistry, ResultRegistry,
    SEARCH_RESPONSE_DEADLINE_BLOCKS, SearchQuery, SearchResult,
};

/// Maximum time the initiating node waits for a UPC acknowledgement.
const UPC_ANNOUNCE_TIMEOUT_MS: u64 = 10_000;

mod bm25;

pub use bm25::search;

state! {
    // Oracle topic used to certify search initiation calls.
    network oracle search;

    // On-chain registries
    network query_registry: QueryRegistry = QueryRegistry::default();
    network result_registry: ResultRegistry = ResultRegistry::default();
    network node_registry: NodeRegistry = NodeRegistry::default();
    // Node-local notifications
    instance ready_queries: Vec<Hash> = Vec::new();
    // Canonical skill bytes persisted in instance state so the in-memory
    // manager can be rebuilt after the Lyquid VM restarts.
    instance skill_blobs: HashMap<Hash, Vec<u8>> = new_hashmap();
}

// Stages the initial search oracle committee at deployment or code upgrade.
//
// The constructor can only stage the committee; a node-side instance call must
// advance and finalize the first epoch before the oracle can certify calls. The
// staging operation is idempotent and is ignored once an epoch is active, so
// re-running it during an upgrade is safe.
// The eth export is required for `shaker deploy -i` to expose the constructor
// ABI and deliver the ABI-encoded committee and threshold.
#[method::network(export = eth)]
fn constructor(ctx: &mut _, committee: Vec<NodeID>, threshold: u16) {
    if let Ok(seq_id) = lyquor_api::sequence_backend_id() {
        let target = OracleTarget {
            seq_id,
            target: OracleServiceTarget::LVM(ctx.lyquid_id),
        };
        let oracle = ctx.network.search.clone();
        let _ = oracle.initialize(&mut ctx, target, committee, threshold);
    }
}

// Registers a node identity through a certified oracle call.
//
// The node's local `/nodes/register` endpoint obtains a certificate for this
// call using its own `ctx.node_id`; the committee attests the request and no
// address-to-key mapping is involved. Registration is idempotent.
#[method::network(group = oracle::certified::search)]
fn register_node(ctx: &mut _, node: NodeID) -> LyquidResult<()> {
    ctx.network.node_registry.register(node)?;
    Ok(())
}

// Removes a node identity through a certified oracle call.
//
// The node's local `/nodes/unregister` endpoint supplies its own `ctx.node_id`.
#[method::network(group = oracle::certified::search)]
fn unregister_node(ctx: &mut _, node: NodeID) -> LyquidResult<()> {
    if !ctx.network.node_registry.unregister(node) {
        return Err(invalid_input("node is not registered"));
    }
    Ok(())
}

// Submits a certified node-registry change from this node's local instance.
//
// The call origin is the Lyquid's own address, not the node identity: the node
// identity travels in the signed input and the certificate proves committee
// approval of that exact call.
fn submit_node_registry_change(
    ctx: &mut crate::__lyquid::InstanceContext,
    method: &str,
    node: NodeID,
) -> LyquidResult<http::Response> {
    let target = OracleTarget {
        seq_id: lyquor_api::sequence_backend_id()?,
        target: OracleServiceTarget::LVM(ctx.lyquid_id),
    };
    let origin = Address::from(ctx.lyquid_id);
    let oracle = ctx.network.search.clone();
    let call = oracle.certify(
        ctx,
        CertifiedCallParams {
            origin,
            method: method.to_owned(),
            input: encode_by_fields!(node: NodeID = node).into(),
            target,
        },
        Bytes::new(),
        None,
        Some(UPC_ANNOUNCE_TIMEOUT_MS),
    )?;

    let Some(call) = call else {
        return Ok(http_json_response(
            503,
            &json_error("node registration certification is unavailable"),
        ));
    };

    match submit_certified_call!(call) {
        Ok(_) => {
            let body = serde_json::json!({
                "status": "submitted",
                "method": method,
                "node_id": node.to_string(),
            })
            .to_string();
            Ok(http_json_response(202, &body))
        }
        Err(error) => Ok(http_json_response(
            502,
            &json_error(&format!("failed to submit certified node change: {error}")),
        )),
    }
}

// Registers this node in Mead's node registry.
//
// The endpoint is instance-only so it can use the node's own `node_id`. It
// requires an active search oracle (the constructor stages the committee and
// the setup script activates it). Repeated calls are safe.
#[method::instance(export = http, method = "POST", path_prefix = "/nodes/register")]
fn register_node_endpoint(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    let node = ctx.node_id;
    submit_node_registry_change(&mut ctx, "register_node", node)
}

// Removes this node from Mead's node registry.
#[method::instance(export = http, method = "POST", path_prefix = "/nodes/unregister")]
fn unregister_node_endpoint(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    let node = ctx.node_id;
    submit_node_registry_change(&mut ctx, "unregister_node", node)
}

// Lists the node identities currently registered on-chain.
#[method::instance(export = http, method = "GET", path_prefix = "/nodes/list")]
fn list_registered_nodes(ctx: &_, _req: http::Request) -> LyquidResult<http::Response> {
    let mut nodes: Vec<NodeID> = ctx.network.node_registry.nodes().iter().copied().collect();
    nodes.sort_by(|left, right| left.0.cmp(&right.0));
    let nodes: Vec<String> = nodes.into_iter().map(|node| node.to_string()).collect();
    let body = serde_json::json!({ "nodes": nodes }).to_string();
    Ok(http_json_response(200, &body))
}

// Registers a query, snapshots the peers expected to answer it, and announces it.
//
// The query carries its sender's node identity and a signature by that node's
// Ed25519 key. A valid signature is the authorization to post the query.
#[method::network]
fn register_query(ctx: &mut _, query: SearchQuery) -> LyquidResult<()> {
    if !verify::verify_signed(&query) {
        return Err(invalid_input("query signature is invalid"));
    }

    let network = &mut *ctx.network;
    plan_search_query(&mut *network.query_registry, &*network.node_registry, query)
}

// Registers a query, snapshots the expected peers, and announces it.
//
// Shared by the generic `register_query` path and the certified search entry
// point. The caller is responsible for authenticating the query first.
fn plan_search_query(
    query_registry: &mut QueryRegistry,
    node_registry: &NodeRegistry,
    query: SearchQuery,
) -> LyquidResult<()> {
    let query_id = query.query_id;
    let initiator = query.sender;

    // Query IDs commit to (digest, k, timestamp, sender) and timestamps have
    // one-second resolution, so two identical searches created in the same
    // second share an ID. Re-registering the same query is an idempotent no-op
    // rather than a failure after the client already received `202 Accepted`.
    if query_registry.contains_identical(&query) {
        return Ok(());
    }

    // `insert` validates the query, including that its text matches the digest,
    // and still rejects a different query that happens to reuse this ID.
    query_registry.insert(query)?;

    // Snapshot the peers at registration time, excluding the initiating node.
    // The same node identities are both the notification targets and the
    // responders whose results the query waits for.
    let mut expected_nodes: Vec<NodeID> = node_registry
        .nodes()
        .iter()
        .copied()
        .filter(|node| *node != initiator)
        .collect();
    expected_nodes.sort();

    let deadline = ChainPos::new(
        lyquor_api::chain_pos()?
            .block()
            .saturating_add(SEARCH_RESPONSE_DEADLINE_BLOCKS),
        0,
    );

    query_registry.plan(query_id, initiator, expected_nodes, deadline)?;

    trigger!(
        announce_search(query_id: HashBytes = HashBytes::from(query_id)),
        TriggerMode::Commit
    );
    Ok(())
}

// Stores a result and triggers completion when every expected peer has answered.
//
// Results reach consensus state only through the certified `coordinate_result`
// entry point. In the deployed instance code, the initiator certifies that call
// only after `submit_peer_result_impl` verified the peer signature, skill bodies,
// Merkle root, and recomputed BM25 scores, and the oracle policy requires the
// sender to be an expected responder. BM25 scores themselves are not
// chain-verifiable (each peer's corpus is local), so the residual trust is "the
// initiator verifies its peers"; clients trust the initiator that ran the search.
fn record_search_result(
    query_registry: &mut QueryRegistry,
    result_registry: &mut ResultRegistry,
    result: SearchResult,
) -> LyquidResult<()> {
    let query = query_registry
        .get(&result.query_id)
        .cloned()
        .ok_or_else(|| LyquidError::LyquidRuntime("query is not registered".into()))?;

    let query_id = result.query_id;
    let sender = result.sender;

    if !query_registry.is_expected_responder(&query_id, sender) {
        return Err(invalid_input(
            "result sender is not an expected responder for this query",
        ));
    }

    result_registry.insert(&query, result)?;
    query_registry.mark_responder(query_id, sender)?;

    let position = lyquor_api::chain_pos()?;
    if query_registry.is_complete(&query_id, position) {
        trigger!(
            check_query_ready(query_id: HashBytes = HashBytes::from(query_id)),
            TriggerMode::Commit
        );
    }

    Ok(())
}

// Marks a query as finished once its response deadline has passed.
//
// Any caller can expire a query after its deadline, so one unresponsive peer
// cannot leave the initiating node waiting forever. The check uses the sequenced
// chain position, which is identical on every node.
#[method::network]
fn expire_query(ctx: &_, query_id: HashBytes) -> LyquidResult<()> {
    let query_id = Hash::from(query_id);

    if !ctx.network.query_registry.contains(&query_id) {
        return Err(invalid_input("query is not registered"));
    }

    let deadline = ctx
        .network
        .query_registry
        .deadline(&query_id)
        .ok_or_else(|| invalid_input("query has no response deadline"))?;

    if lyquor_api::chain_pos()? < deadline {
        return Err(invalid_input("query response deadline has not passed"));
    }

    trigger!(
        check_query_ready(query_id: HashBytes = HashBytes::from(query_id)),
        TriggerMode::Commit
    );
    Ok(())
}

// Runs on the initiating node after the query slot commits, and notifies peers.
//
// The commit trigger fires on every node that executed the network call, so the
// `node_id` check keeps the UPC from being sent once per node.
#[method::instance]
fn announce_search(ctx: &mut _, query_id: HashBytes) -> LyquidResult<()> {
    let query_id = Hash::from(query_id);

    let Some(initiator) = ctx.network.query_registry.initiator_node(&query_id) else {
        return Ok(());
    };
    if ctx.node_id != initiator {
        return Ok(());
    }

    let expected_nodes: Vec<NodeID> = ctx
        .network
        .query_registry
        .expected_nodes(&query_id)
        .map(|nodes| nodes.to_vec())
        .unwrap_or_default();

    // Nothing to wait for, so the query is already ready on this node.
    if expected_nodes.is_empty() {
        let mut ready = ctx.instance.ready_queries.write();
        if !ready.contains(&query_id) {
            ready.push(query_id);
        }
        return Ok(());
    }

    let query = ctx
        .network
        .query_registry
        .get(&query_id)
        .ok_or_else(|| invalid_input("query is not registered"))?;
    if query.query_text.is_empty() {
        return Err(invalid_input("query text is not available on-chain"));
    }

    lyquid::println!(
        "mead: announcing search {query_id} to {} peers",
        expected_nodes.len()
    );
    let input = encode_by_fields!(query: SearchQuery = query.clone());
    let response = lyquor_api::universal_procedural_call(
        ctx.lyquid_id,
        None,
        "search_request".into(),
        Vec::from(&input[..]),
        Some(input.into()),
        Some(UPC_ANNOUNCE_TIMEOUT_MS),
    )?;
    let reply = decode_by_fields!(&response, reply: SearchReply)
        .ok_or_else(|| invalid_input("search peers returned an invalid reply"))?;
    lyquid::println!(
        "mead: initiator received {} peer replies for {query_id}",
        reply.reply.replies.len()
    );
    for peer_reply in reply.reply.replies {
        let sender = peer_reply.result.sender;
        if let Err(error) = submit_peer_result_impl(&mut ctx, peer_reply) {
            lyquid::println!("mead: failed to submit result for {query_id} from {sender}: {error}");
        }
    }

    Ok(())
}

// UPC prepare step: selects the peers that must be notified about this query.
#[method::instance(upc(prepare))]
fn search_request(ctx: &_, query: SearchQuery) -> LyquidResult<Vec<NodeID>> {
    let query_id = query.query_id;
    Ok(ctx
        .network
        .query_registry
        .expected_nodes(&query_id)
        .map(|nodes| nodes.to_vec())
        .unwrap_or_default())
}

// UPC request step: runs on each selected peer, searches locally, and returns
// the peer-signed result to the initiator.
#[method::instance(upc(request))]
fn search_request(ctx: &mut _, query: SearchQuery) -> LyquidResult<SearchReply> {
    let query_id = query.query_id;
    lyquid::println!("mead: peer {} handling search {query_id}", ctx.node_id);

    if query.query_text.is_empty()
        || blake3::hash(query.query_text.as_bytes()) != query.query_digest
    {
        return Err(invalid_input("query text does not match the query digest"));
    }
    if !verify::verify_signed(&query) {
        return Err(invalid_input("query signature is invalid"));
    }

    let blobs = ctx.instance.skill_blobs.read();
    ensure_skills_loaded_from(&blobs)?;
    drop(blobs);

    let skills: Vec<Skill> = match SkillManager::shared() {
        Ok(manager) => manager.iter().map(|(_, skill)| (*skill).clone()).collect(),
        Err(SkillError::NotInitialized) => Vec::new(),
        Err(error) => return Err(invalid_input(error.to_string())),
    };

    let matches = bm25::search(&query.query_text, query.params.k as usize, &skills);
    lyquid::println!(
        "mead: peer {} local search {query_id}: {} skills -> {} matches",
        ctx.node_id,
        skills.len(),
        matches.len()
    );
    let timestamp = lyquor_api::systime()? / 1000;
    let sender = ctx.node_id;

    let mut result = SearchResult::new(query_id, matches, timestamp, sender)?;
    let signature = lyquor_api::sign(Bytes::from(result.signing_payload()), Cipher::Ed25519)?;
    result.set_signature(Vec::from(signature.as_ref()));

    let manager = SkillManager::shared().map_err(|error| invalid_input(error.to_string()))?;
    let mut skill_bodies = Vec::with_capacity(result.matches.len());
    for skill_match in &result.matches {
        let skill = manager
            .get(&skill_match.skill_digest)
            .ok_or_else(|| invalid_input("matched skill is not loaded on this node"))?;
        skill_bodies.push(
            skill
                .canonical_bytes()
                .map_err(|error| invalid_input(error.to_string()))?,
        );
    }

    Ok(SearchReply {
        replies: vec![PeerSearchReply {
            result,
            skill_bodies,
        }],
    })
}

// UPC response step: runs on the initiator and submits each peer-signed result
// as it arrives.
#[method::instance(upc(response))]
fn search_request(
    ctx: &_,
    response: LyquidResult<SearchReply>,
) -> LyquidResult<Option<SearchReply>> {
    let reply = response?;
    let Some(first) = reply.replies.first() else {
        return Ok(None);
    };

    let expected = ctx
        .network
        .query_registry
        .expected_peer_count(&first.result.query_id);
    let cache = ctx.cache.get_mut_or_init(|| SearchReplyCache {
        expected,
        replies: Vec::new(),
    });
    cache.replies.extend(reply.replies);

    if cache.replies.len() >= cache.expected {
        let replies = std::mem::take(&mut cache.replies);
        Ok(Some(SearchReply { replies }))
    } else {
        Ok(None)
    }
}

// Verifies a peer-signed result and submits it through the search oracle.
//
// The certificate is issued to this initiator, not the peer: it authorizes this
// node to record the result. Authenticity still comes from the peer's own Ed25519
// signature over the result, so a certificate cannot forge or alter it. In the
// deployed instance code, every result certified for `coordinate_result` has
// passed these local checks first.
fn submit_peer_result_impl(
    ctx: &mut crate::__lyquid::InstanceContext,
    peer_reply: PeerSearchReply,
) -> LyquidResult<()> {
    let PeerSearchReply {
        result,
        skill_bodies,
    } = peer_reply;
    if ctx.network.query_registry.initiator_node(&result.query_id) != Some(ctx.node_id) {
        return Ok(());
    }
    if !verify::verify_signed(&result) {
        return Err(invalid_input("result signature is invalid"));
    }

    let query = ctx
        .network
        .query_registry
        .get(&result.query_id)
        .cloned()
        .ok_or_else(|| invalid_input("query is not registered"))?;
    result.validate_against(&query)?;
    if skill_bodies.len() != result.matches.len() {
        return Err(invalid_input(
            "result does not include a body for every matched skill",
        ));
    }

    let mut matched_skills = Vec::with_capacity(result.matches.len());
    for (skill_match, body) in result.matches.iter().zip(skill_bodies.iter()) {
        let skill =
            Skill::from_canonical_bytes(body).map_err(|error| invalid_input(error.to_string()))?;
        let digest = skill
            .digest()
            .map_err(|error| invalid_input(error.to_string()))?;
        if digest != skill_match.skill_digest {
            return Err(invalid_input("skill body does not match the match digest"));
        }
        matched_skills.push(skill);
    }

    let recomputed: HashMap<Hash, Bm25Score> =
        bm25::score_corpus(&query.query_text, &matched_skills)
            .into_iter()
            .collect();
    if recomputed.len() != result.matches.len() {
        return Err(invalid_input("skill corpus does not match the result"));
    }
    for skill_match in &result.matches {
        if recomputed.get(&skill_match.skill_digest) != Some(&skill_match.bm25_score) {
            return Err(invalid_input(
                "recomputed BM25 score does not match the result",
            ));
        }
    }

    let target = OracleTarget {
        seq_id: lyquor_api::sequence_backend_id()?,
        target: OracleServiceTarget::LVM(ctx.lyquid_id),
    };
    let origin = Address::from(ctx.lyquid_id);
    let oracle = ctx.network.search.clone();
    let call = oracle.certify(
        ctx,
        CertifiedCallParams {
            origin,
            method: "coordinate_result".into(),
            input: encode_by_fields!(result: SearchResult = result).into(),
            target,
        },
        Bytes::new(),
        None,
        Some(UPC_ANNOUNCE_TIMEOUT_MS),
    )?;

    let Some(call) = call else {
        return Err(invalid_input("result certification is unavailable"));
    };
    submit_certified_call!(call)?;
    Ok(())
}

// Accepts a certified search initiation and registers the query on-chain.
//
// The certificate proves that the search oracle committee approved this exact
// query, while the query keeps its own NodeID signature so peers can verify it
// independently. The certificate origin is the calling Lyquid, not the search
// identity, so it is intentionally not compared with `query.sender`.
#[method::network(group = oracle::certified::search)]
fn coordinate_search(ctx: &mut _, query: SearchQuery) -> LyquidResult<()> {
    if !verify::verify_signed(&query) {
        return Err(invalid_input("query signature is invalid"));
    }

    lyquid::println!("mead: coordinate_search {}", query.query_id);
    let network = &mut *ctx.network;
    plan_search_query(&mut *network.query_registry, &*network.node_registry, query)
}

// Accepts a certified peer result and records it on-chain.
//
// The initiating node calls this only after `submit_peer_result_impl` verified
// the peer's signature, the transmitted skill bodies, the Merkle root, and the
// recomputed BM25 scores. On-chain, the oracle policy additionally requires the
// sender to be one of the query's expected responders, so consensus state cannot
// accept a result from an unassigned node. BM25 scores themselves cannot be
// re-verified on-chain because each peer scores against its own local corpus;
// clients trust the initiator that ran the search.
#[method::network(group = oracle::certified::search)]
fn coordinate_result(ctx: &mut _, result: SearchResult) -> LyquidResult<()> {
    if !verify::verify_signed(&result) {
        return Err(invalid_input("result signature is invalid"));
    }

    let network = &mut *ctx.network;
    lyquid::println!(
        "mead: recording result for {} from {}",
        result.query_id,
        result.sender
    );
    record_search_result(
        &mut *network.query_registry,
        &mut *network.result_registry,
        result,
    )
}

// Oracle policy hook: approves certified search calls for query creation and
// peer result submission.
#[method::instance(group = oracle::single_phase::search)]
fn validate(
    ctx: &mut _,
    params: CallParams,
    _extra: Bytes,
    target: OracleTarget,
) -> LyquidResult<bool> {
    let expected_target = OracleTarget {
        seq_id: lyquor_api::sequence_backend_id()?,
        target: OracleServiceTarget::LVM(ctx.lyquid_id),
    };

    if target != expected_target
        || params.group != "search"
        || params.abi != InputABI::Lyquor
        || params.origin == Address::ZERO
        || params.caller != Address::from(ctx.lyquid_id)
    {
        return Ok(false);
    }

    match params.method.as_str() {
        "coordinate_search" => {
            let Some(input) = decode_by_fields!(&params.input, query: SearchQuery) else {
                return Ok(false);
            };
            if input.query.query_text.is_empty()
                || blake3::hash(input.query.query_text.as_bytes()) != input.query.query_digest
            {
                return Ok(false);
            }
            Ok(verify::verify_signed(&input.query))
        }
        "coordinate_result" => {
            let Some(input) = decode_by_fields!(&params.input, result: SearchResult) else {
                return Ok(false);
            };
            if !verify::verify_signed(&input.result) {
                return Ok(false);
            }
            // Enforce the responder-set invariant in the policy too, so a buggy
            // or hostile instance cannot certify an unassigned result even
            // though the deployed code path already checks it.
            Ok(ctx
                .network
                .query_registry
                .is_expected_responder(&input.result.query_id, input.result.sender))
        }
        "register_node" | "unregister_node" => {
            let Some(input) = decode_by_fields!(&params.input, node: NodeID) else {
                return Ok(false);
            };
            Ok(input.node.0 != [0u8; 32])
        }
        _ => Ok(false),
    }
}

// Checks whether a query is complete and records it locally on the initiating node.
//
// Triggered after a result submission or an expiry, and also callable on demand so
// an initiating node can recover a completion signal it missed (for example after
// a restart).
#[method::instance]
fn check_query_ready(ctx: &mut _, query_id: HashBytes) -> LyquidResult<()> {
    let query_id = Hash::from(query_id);

    let position = lyquor_api::chain_pos()?;
    if !ctx.network.query_registry.is_complete(&query_id, position) {
        return Ok(());
    }
    if ctx.network.query_registry.initiator_node(&query_id) != Some(ctx.node_id) {
        return Ok(());
    }

    let mut ready = ctx.instance.ready_queries.write();
    if !ready.contains(&query_id) {
        ready.push(query_id);
    }
    Ok(())
}

// Returns the queries whose results are complete on the initiating node.
#[method::instance]
fn get_ready_queries(ctx: &_) -> LyquidResult<Vec<HashBytes>> {
    Ok(ctx
        .instance
        .ready_queries
        .read()
        .iter()
        .copied()
        .map(HashBytes::from)
        .collect())
}

// Removes a handled query from this node's ready list.
#[method::instance]
fn ack_ready_query(ctx: &mut _, query_id: HashBytes) -> LyquidResult<()> {
    let query_id = Hash::from(query_id);
    ctx.instance
        .ready_queries
        .write()
        .retain(|ready| *ready != query_id);
    Ok(())
}

// Returns a registered query by its canonical ID.
#[method::network]
fn get_query(ctx: &_, query_id: HashBytes) -> LyquidResult<Option<SearchQuery>> {
    let query_id = Hash::from(query_id);
    Ok(ctx.network.query_registry.get(&query_id).cloned())
}

// Returns a peer's result for a query.
#[method::network]
fn get_result(ctx: &_, query_id: HashBytes, sender: NodeID) -> LyquidResult<Option<SearchResult>> {
    let query_id = Hash::from(query_id);
    Ok(ctx.network.result_registry.get(&query_id, sender).cloned())
}

// Returns the sorted responders registered for a query.
#[method::network]
fn get_responders(ctx: &_, query_id: HashBytes) -> LyquidResult<Vec<NodeID>> {
    let query_id = Hash::from(query_id);
    let mut responders: Vec<NodeID> = ctx
        .network
        .query_registry
        .responders(&query_id)
        .map(|nodes| nodes.iter().copied().collect())
        .unwrap_or_default();
    responders.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(responders)
}

// Returns the number of skills currently registered on this node.
#[method::instance(export = http, method = "GET", path_prefix = "/skills/count")]
fn get_skill_count(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    ensure_skills_loaded(&ctx)?;
    skill_count_response()
}

// Creates a new search query from this node.
//
// The endpoint builds and signs a `SearchQuery` from the node's own identity,
// then submits a certified call to the search oracle. The certified network
// method registers the query and triggers the peer-coordination UPC.
#[method::instance(export = http, method = "POST", path_prefix = "/search/new")]
fn search_new_endpoint(ctx: &mut _, req: http::Request) -> LyquidResult<http::Response> {
    let request = match parse_search_endpoint_request(req.body.as_deref()) {
        Ok(request) => request,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    let params = QueryParams { k: request.k };
    if let Err(error) = params.validate() {
        return Ok(http_json_response(400, &json_error(&error.to_string())));
    }

    let sender = ctx.node_id;

    let query_text = request.query;
    let timestamp = lyquor_api::systime()? / 1000;
    let mut query = match SearchQuery::new(query_text.as_bytes(), params, timestamp, sender) {
        Ok(query) => query,
        Err(error) => return Ok(http_json_response(400, &json_error(&error.to_string()))),
    };

    let signature = lyquor_api::sign(Bytes::from(query.signing_payload()), Cipher::Ed25519)?;
    let signature: Vec<u8> = Vec::from(signature.as_ref());
    query.set_signature(signature);

    let query_id = query.query_id;
    let target = OracleTarget {
        seq_id: lyquor_api::sequence_backend_id()?,
        target: OracleServiceTarget::LVM(ctx.lyquid_id),
    };
    let origin = Address::from(ctx.lyquid_id);
    let oracle = ctx.network.search.clone();
    let call = match oracle.certify(
        &mut ctx,
        CertifiedCallParams {
            origin,
            method: "coordinate_search".into(),
            input: encode_by_fields!(query: SearchQuery = query).into(),
            target,
        },
        Bytes::new(),
        None,
        Some(UPC_ANNOUNCE_TIMEOUT_MS),
    ) {
        Ok(Some(call)) => call,
        Ok(None) => {
            return Ok(http_json_response(
                503,
                &json_error("search certification is unavailable"),
            ));
        }
        Err(error) => return Ok(http_json_response(500, &json_error(&error.to_string()))),
    };

    match submit_certified_call!(call) {
        Ok(_) => Ok(search_accepted_response(query_id)),
        Err(error) => Ok(http_json_response(
            502,
            &json_error(&format!("failed to submit certified search: {error}")),
        )),
    }
}

// Reports whether a search has completed and how many peer results arrived.
#[method::instance(export = http, method = "POST", path_prefix = "/search/status")]
fn search_status_endpoint(ctx: &_, req: http::Request) -> LyquidResult<http::Response> {
    let query_id = match parse_query_id_request(req.body.as_deref()) {
        Ok(query_id) => query_id,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    if ctx.network.query_registry.get(&query_id).is_none() {
        return Ok(http_json_response(
            404,
            &json_error("query is not registered"),
        ));
    }

    let position = lyquor_api::chain_pos()?;
    let completed = ctx.network.query_registry.is_complete(&query_id, position);
    let received_results = ctx.network.result_registry.count_for_query(&query_id);
    let expected_results = ctx.network.query_registry.expected_peer_count(&query_id);
    let timed_out = ctx
        .network
        .query_registry
        .deadline_passed(&query_id, position)
        && received_results < expected_results;

    Ok(search_status_response(
        query_id,
        completed,
        received_results,
        expected_results,
        timed_out,
    ))
}

// Returns the peer results for a completed search query.
//
// On the initiating node the response also includes a locally computed result,
// so the initiator contributes its own matches. Any other node serves only the
// on-chain peer results and does not run a query it did not start.
#[method::instance(export = http, method = "POST", path_prefix = "/search/get")]
fn search_get_endpoint(ctx: &_, req: http::Request) -> LyquidResult<http::Response> {
    let query_id = match parse_query_id_request(req.body.as_deref()) {
        Ok(query_id) => query_id,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    if ctx.network.query_registry.get(&query_id).is_none() {
        return Ok(http_json_response(
            404,
            &json_error("query is not registered"),
        ));
    }

    let position = lyquor_api::chain_pos()?;
    let completed = ctx.network.query_registry.is_complete(&query_id, position);
    let results = ctx.network.result_registry.for_query(&query_id);

    if !completed {
        let expected_results = ctx.network.query_registry.expected_peer_count(&query_id);
        return Ok(search_pending_response(
            query_id,
            results.len(),
            expected_results,
        ));
    }

    let query = ctx
        .network
        .query_registry
        .get(&query_id)
        .cloned()
        .ok_or_else(|| invalid_input("query is not registered"))?;

    let is_initiator = ctx.network.query_registry.initiator_node(&query_id) == Some(ctx.node_id);
    let local_result = if is_initiator {
        Some(local_search_result(&ctx, &query)?)
    } else {
        None
    };

    let mut combined = results;
    if let Some(local_result) = local_result.as_ref() {
        combined.push(local_result);
    }

    let expected_results = ctx.network.query_registry.expected_peer_count(&query_id);
    let received_results = ctx.network.query_registry.responder_count(&query_id);
    let timed_out = ctx
        .network
        .query_registry
        .deadline_passed(&query_id, position)
        && received_results < expected_results;

    Ok(search_results_response(query_id, &combined, timed_out))
}

// Runs the query against this node's own skill catalog when the initiator
// fetches the final results. The local result is returned to the caller but is
// not written to the on-chain result registry.
fn local_search_result(
    ctx: &crate::__lyquid::ImmutableInstanceContext,
    query: &SearchQuery,
) -> LyquidResult<SearchResult> {
    ensure_skills_loaded_immutable(ctx)?;
    let skills: Vec<Skill> = match SkillManager::shared() {
        Ok(manager) => manager.iter().map(|(_, skill)| (*skill).clone()).collect(),
        Err(SkillError::NotInitialized) => Vec::new(),
        Err(error) => return Err(invalid_input(error.to_string())),
    };

    let matches = bm25::search(&query.query_text, query.params.k as usize, &skills);
    let timestamp = lyquor_api::systime()? / 1000;
    let sender = ctx.node_id;

    let mut result = SearchResult::new(query.query_id, matches, timestamp, sender)?;
    let signature = lyquor_api::sign(Bytes::from(result.signing_payload()), Cipher::Ed25519)?;
    result.set_signature(Vec::from(signature.as_ref()));
    Ok(result)
}

// Returns the name, description, and digest of every skill registered on this
// node.
#[method::instance(export = http, method = "GET", path_prefix = "/skills/list")]
fn list_skills(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    ensure_skills_loaded(&ctx)?;
    let body = match SkillManager::shared() {
        Ok(manager) => skill_list_json(manager),
        Err(SkillError::NotInitialized) => "[]".to_owned(),
        Err(error) => return Ok(http_json_response(500, &json_error(&error.to_string()))),
    };
    Ok(http_json_response(200, &body))
}

// Accepts a single JSON skill object, validates it, and registers it with the
// node-local skill manager.
//
// The object uses the same representation as `Skill`'s serde implementation.
// The JSON transport form produced by `Skill::to_json` is accepted as well, so
// clients may send resource contents without precomputing their digests.
// Upload endpoint under `/skills`.
#[method::instance(export = http, method = "POST", path_prefix = "/skills/upload")]
fn upload_skills_at_skills_upload(ctx: &mut _, req: http::Request) -> LyquidResult<http::Response> {
    handle_skill_upload(&mut ctx, req)
}

#[cfg(test)]
mod tests;
