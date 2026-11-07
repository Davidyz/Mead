//! Mead: blockchain-based P2P skill distribution platform.
//!
//! On-chain state consists of a skill directory and query/result registries
//! that track search queries and peer-submitted results.

use lyquid::prelude::lyquor_primitives::{Cipher, HashBytes, InputABI};
use lyquid::prelude::*;
use lyquid::println;
use std::sync::Arc;

pub mod skill;
pub mod types;
pub mod verify;

use skill::{Skill, SkillError, SkillManager};
use types::invalid_input;
use types::{
    Bm25Score, CanonicalBytes, NodeRegistry, QueryParams, QueryRegistry, ResultRegistry,
    SEARCH_RESPONSE_DEADLINE_BLOCKS, SearchQuery, SearchResult, SkillMatch,
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
    instance pending_searches: Vec<Hash> = Vec::new();
    instance ready_queries: Vec<Hash> = Vec::new();
    instance processed_searches: HashSet<Hash> = new_hashset();
    // Canonical skill bytes persisted in instance state so the in-memory
    // manager can be rebuilt after the Lyquid VM restarts.
    instance skill_blobs: HashMap<Hash, Vec<u8>> = new_hashmap();
}

// Registers the caller's node identity so it can receive search notifications.
#[method::network]
fn register_node(ctx: &mut _) -> LyquidResult<()> {
    let node = lyquor_api::get_ed25519_by_address(ctx.caller)
        .ok()
        .flatten()
        .ok_or_else(|| invalid_input("caller has no ed25519 identity"))?;

    ctx.network.node_registry.register(node)?;
    Ok(())
}

// Registers a node identity supplied by tooling or a deployment client.
//
// The bartender mapping must bind `address` to `node`, so a caller cannot
// register an arbitrary identity that does not belong to that address.
#[method::network(export = eth)]
fn register_node_address(ctx: &mut _, node: NodeID, address: Address) -> LyquidResult<()> {
    if address == Address::ZERO {
        return Err(invalid_input("node address must not be zero"));
    }

    let resolved = lyquor_api::get_ed25519_by_address(address).ok().flatten();
    if resolved != Some(node) {
        return Err(invalid_input(
            "address is not bound to the supplied node identity",
        ));
    }

    ctx.network.node_registry.register(node)?;
    Ok(())
}

// Removes the caller's own node identity from the registry.
//
// The identity is derived from the caller, so a node can only unregister itself.
#[method::network]
fn unregister_node(ctx: &mut _) -> LyquidResult<()> {
    let node = lyquor_api::get_ed25519_by_address(ctx.caller)
        .ok()
        .flatten()
        .ok_or_else(|| invalid_input("caller has no ed25519 identity"))?;

    if !ctx.network.node_registry.unregister(node) {
        return Err(invalid_input("node is not registered"));
    }
    Ok(())
}

// Registers a query, snapshots the peers expected to answer it, and announces it.
//
// The signature must be valid and the caller must be authorized to submit on
// behalf of the declared sender, otherwise any node could post a query under a
// forged address and have it accepted.
#[method::network]
fn register_query(ctx: &mut _, query: SearchQuery) -> LyquidResult<()> {
    verify::ensure_authorized(ctx.caller, query.sender)?;

    if !verify::verify_signed(&query) {
        return Err(invalid_input("query signature is invalid"));
    }

    let initiator = lyquor_api::get_ed25519_by_address(query.sender)
        .ok()
        .flatten()
        .ok_or_else(|| invalid_input("query sender has no ed25519 identity"))?;

    let network = &mut *ctx.network;
    plan_search_query(
        &mut *network.query_registry,
        &*network.node_registry,
        query,
        initiator,
    )
}

// Registers a query, snapshots the expected peers, and announces it.
//
// Shared by the generic `register_query` path and the certified search entry
// point. The caller is responsible for authenticating the query first.
fn plan_search_query(
    query_registry: &mut QueryRegistry,
    node_registry: &NodeRegistry,
    query: SearchQuery,
    initiator: NodeID,
) -> LyquidResult<()> {
    let query_id = query.query_id;

    if !query.query_text.is_empty()
        && blake3::hash(query.query_text.as_bytes()) != query.query_digest
    {
        return Err(invalid_input("query text does not match the query digest"));
    }

    query_registry.insert(query)?;

    // Snapshot the peers at registration time, excluding the initiating node.
    let mut expected_nodes: Vec<NodeID> = node_registry
        .nodes()
        .iter()
        .copied()
        .filter(|node| *node != initiator)
        .collect();
    expected_nodes.sort();

    let mut expected_responders: HashSet<Address> = new_hashset();
    expected_nodes.retain(|node| match lyquor_api::get_address_by_ed25519(node.0) {
        Ok(Some(address)) => {
            expected_responders.insert(address);
            true
        }
        _ => false,
    });

    let deadline = ChainPos::new(
        lyquor_api::chain_pos()?
            .block()
            .saturating_add(SEARCH_RESPONSE_DEADLINE_BLOCKS),
        0,
    );

    query_registry.plan(
        query_id,
        initiator,
        expected_nodes,
        expected_responders,
        deadline,
    )?;

    trigger!(
        announce_search(query_id: HashBytes = HashBytes::from(query_id)),
        TriggerMode::Commit
    );
    Ok(())
}

// Validates and stores a peer result, then records the peer as a responder.
#[method::network]
fn submit_result(ctx: &mut _, result: SearchResult) -> LyquidResult<()> {
    verify::ensure_authorized(ctx.caller, result.sender)?;

    if !verify::verify_signed(&result) {
        return Err(invalid_input("result signature is invalid"));
    }

    let network = &mut *ctx.network;
    record_search_result(
        &mut *network.query_registry,
        &mut *network.result_registry,
        result,
    )
}

// Stores a result and triggers completion when every expected peer has answered.
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

    let query_text = ctx
        .network
        .query_registry
        .get(&query_id)
        .map(|query| query.query_text.clone())
        .ok_or_else(|| invalid_input("query is not registered"))?;
    if query_text.is_empty() {
        return Err(invalid_input("query text is not available on-chain"));
    }

    lyquid::println!(
        "mead: announcing search {query_id} to {} peers",
        expected_nodes.len()
    );
    let input = encode_by_fields!(
        query_id: HashBytes = HashBytes::from(query_id),
        query_text: String = query_text
    );
    lyquor_api::universal_procedural_call(
        ctx.lyquid_id,
        None,
        "search_request".into(),
        Vec::from(&input[..]),
        Some(input.into()),
        Some(UPC_ANNOUNCE_TIMEOUT_MS),
    )?;

    Ok(())
}

// UPC prepare step: selects the peers that must be notified about this query.
#[method::instance(upc(prepare))]
fn search_request(ctx: &_, query_id: HashBytes) -> LyquidResult<Vec<NodeID>> {
    let query_id = Hash::from(query_id);
    Ok(ctx
        .network
        .query_registry
        .expected_nodes(&query_id)
        .map(|nodes| nodes.to_vec())
        .unwrap_or_default())
}

// UPC request step: runs on each selected peer and schedules local processing.
#[method::instance(upc(request))]
fn search_request(ctx: &mut _, query_id: HashBytes, query_text: String) -> LyquidResult<bool> {
    let query_id = Hash::from(query_id);

    let query = ctx
        .network
        .query_registry
        .get(&query_id)
        .cloned()
        .ok_or_else(|| invalid_input("query is not registered"))?;

    if blake3::hash(query_text.as_bytes()) != query.query_digest {
        return Err(invalid_input(
            "query text does not match the on-chain digest",
        ));
    }

    {
        let mut pending = ctx.instance.pending_searches.write();
        if !pending.contains(&query_id) {
            pending.push(query_id);
        }
    }
    lyquid::println!("mead: peer {} received search {query_id}", ctx.node_id);
    Ok(true)
}

// Runs the local BM25 search for an assigned query and submits the result.
#[method::instance]
fn process_assigned_search(ctx: &mut _, query_id: HashBytes) -> LyquidResult<()> {
    process_assigned_search_impl(&mut ctx, Hash::from(query_id))
}

fn process_assigned_search_impl(
    ctx: &mut crate::__lyquid::InstanceContext,
    query_id: Hash,
) -> LyquidResult<()> {
    if ctx.network.query_registry.initiator_node(&query_id) == Some(ctx.node_id) {
        return Ok(());
    }

    let assigned = ctx
        .network
        .query_registry
        .expected_nodes(&query_id)
        .is_some_and(|nodes| nodes.contains(&ctx.node_id));
    if !assigned || ctx.instance.processed_searches.read().contains(&query_id) {
        return Ok(());
    }

    let Some(query) = ctx.network.query_registry.get(&query_id).cloned() else {
        return Ok(());
    };
    let query_text = query.query_text.clone();
    if query_text.is_empty() {
        return Ok(());
    }
    if blake3::hash(query_text.as_bytes()) != query.query_digest {
        return Err(invalid_input(
            "query text does not match the on-chain digest",
        ));
    }

    ensure_skills_loaded(ctx)?;
    let skills: Vec<Skill> = match SkillManager::shared() {
        Ok(manager) => manager.iter().map(|(_, skill)| (*skill).clone()).collect(),
        Err(SkillError::NotInitialized) => Vec::new(),
        Err(error) => return Err(invalid_input(error.to_string())),
    };

    let matches = bm25::search(&query_text, query.params.k as usize, &skills);
    lyquid::println!(
        "mead: peer {} searched {query_id}: {} skills -> {} matches",
        ctx.node_id,
        skills.len(),
        matches.len()
    );
    let timestamp = lyquor_api::systime()? / 1000;
    let sender = lyquor_api::get_address_by_ed25519(ctx.node_id.0)
        .ok()
        .flatten()
        .ok_or_else(|| invalid_input("node has no on-chain address"))?;

    let mut result = SearchResult::new(query_id, matches, timestamp, sender)?;
    let signature = lyquor_api::sign(Bytes::from(result.signing_payload()), Cipher::Ed25519)?;
    let signature: Vec<u8> = Vec::from(signature.as_ref());
    result.set_signature(signature);

    let target = OracleTarget {
        seq_id: lyquor_api::sequence_backend_id()?,
        target: OracleServiceTarget::LVM(ctx.lyquid_id),
    };
    let oracle = ctx.network.search.clone();
    let call = oracle.certify(
        ctx,
        CertifiedCallParams {
            origin: sender,
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
    lyquid::println!("mead: peer {} submitted result for {query_id}", ctx.node_id);

    ctx.instance.processed_searches.write().insert(query_id);
    ctx.instance
        .pending_searches
        .write()
        .retain(|pending| *pending != query_id);
    Ok(())
}

// Accepts a certified search initiation and registers the query on-chain.
//
// The certificate proves that the search oracle committee approved this exact
// query for this sender. The query keeps its own signature so peers can verify
// it independently, and the certificate origin is bound to the query sender so
// a valid certificate cannot be replayed for a different sender address.
#[method::network(group = oracle::certified::search)]
fn coordinate_search(ctx: &mut _, query: SearchQuery) -> LyquidResult<()> {
    if query.sender != ctx.origin {
        return Err(invalid_input(
            "query sender does not match the certificate origin",
        ));
    }
    if !verify::verify_signed(&query) {
        return Err(invalid_input("query signature is invalid"));
    }

    let initiator = lyquor_api::get_ed25519_by_address(query.sender)
        .ok()
        .flatten()
        .ok_or_else(|| invalid_input("query sender has no ed25519 identity"))?;

    lyquid::println!("mead: coordinate_search {}", query.query_id);
    let network = &mut *ctx.network;
    plan_search_query(
        &mut *network.query_registry,
        &*network.node_registry,
        query,
        initiator,
    )
}

// Accepts a certified peer result and records it on-chain.
#[method::network(group = oracle::certified::search)]
fn coordinate_result(ctx: &mut _, result: SearchResult) -> LyquidResult<()> {
    if result.sender != ctx.origin {
        return Err(invalid_input(
            "result sender does not match the certificate origin",
        ));
    }
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
            if blake3::hash(input.query.query_text.as_bytes()) != input.query.query_digest {
                return Ok(false);
            }
            if input.query.sender != params.origin {
                return Ok(false);
            }
            Ok(verify::verify_signed(&input.query))
        }
        "coordinate_result" => {
            let Some(input) = decode_by_fields!(&params.input, result: SearchResult) else {
                return Ok(false);
            };
            if input.result.sender != params.origin {
                return Ok(false);
            }
            Ok(verify::verify_signed(&input.result))
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

// Returns the queries this node has been asked to process.
#[method::instance]
fn get_pending_searches(ctx: &_) -> LyquidResult<Vec<HashBytes>> {
    Ok(ctx
        .instance
        .pending_searches
        .read()
        .iter()
        .copied()
        .map(HashBytes::from)
        .collect())
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

// Removes a handled query from this node's pending list.
#[method::instance]
fn ack_pending_search(ctx: &mut _, query_id: HashBytes) -> LyquidResult<()> {
    let query_id = Hash::from(query_id);
    ctx.instance
        .pending_searches
        .write()
        .retain(|pending| *pending != query_id);
    Ok(())
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
fn get_result(ctx: &_, query_id: HashBytes, sender: Address) -> LyquidResult<Option<SearchResult>> {
    let query_id = Hash::from(query_id);
    Ok(ctx.network.result_registry.get(&query_id, sender).cloned())
}

// Returns the sorted responders registered for a query.
#[method::network]
fn get_responders(ctx: &_, query_id: HashBytes) -> LyquidResult<Vec<Address>> {
    let query_id = Hash::from(query_id);
    let mut responders: Vec<Address> = ctx
        .network
        .query_registry
        .responders(&query_id)
        .map(|addresses| addresses.iter().copied().collect())
        .unwrap_or_default();
    responders.sort_by(|left, right| left.as_slice().cmp(right.as_slice()));
    Ok(responders)
}

// Returns the number of skills currently registered on this node.
#[method::instance(export = http, method = "GET", path_prefix = "/skills/count")]
fn get_skill_count(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    ensure_skills_loaded(&mut ctx)?;
    skill_count_response()
}

fn skill_count_response() -> LyquidResult<http::Response> {
    let count = SkillManager::shared()
        .map(|manager| manager.len())
        .unwrap_or_default();
    println!("mead: skill count requested: count={count}");
    Ok(skill_count_http_response(count))
}

fn skill_count_http_response(count: usize) -> http::Response {
    let body = serde_json::json!({ "count": count }).to_string();
    http_json_response(200, &body)
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

    let sender = match lyquor_api::get_address_by_ed25519(ctx.node_id.0) {
        Ok(Some(sender)) => sender,
        _ => {
            return Ok(http_json_response(
                500,
                &json_error("node has no on-chain address"),
            ));
        }
    };

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
    let oracle = ctx.network.search.clone();
    let call = match oracle.certify(
        &mut ctx,
        CertifiedCallParams {
            origin: sender,
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

    Ok(search_status_response(
        query_id,
        completed,
        received_results,
        expected_results,
    ))
}

// Returns the peer results for a completed search query.
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

    Ok(search_results_response(query_id, &results))
}

// Processes an assigned query on this peer and submits its result on-chain.
#[method::instance(export = http, method = "POST", path_prefix = "/search/process")]
fn search_process_endpoint(ctx: &mut _, req: http::Request) -> LyquidResult<http::Response> {
    let query_id = match parse_query_id_request(req.body.as_deref()) {
        Ok(query_id) => query_id,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    match process_assigned_search_impl(&mut ctx, query_id) {
        Ok(()) => {
            let body = serde_json::json!({
                "status": "processed",
                "query_id": query_id.to_string(),
            })
            .to_string();
            Ok(http_json_response(200, &body))
        }
        Err(error) => Ok(http_json_response(500, &json_error(&error.to_string()))),
    }
}

#[derive(Debug, serde::Deserialize)]
struct SearchEndpointRequest {
    query: String,
    #[serde(default = "default_results_per_peer")]
    k: u64,
}

fn default_results_per_peer() -> u64 {
    10
}

fn parse_search_endpoint_request(body: Option<&[u8]>) -> Result<SearchEndpointRequest, String> {
    let body = body.ok_or_else(|| "request body is required".to_owned())?;
    let request: SearchEndpointRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid search request JSON: {error}"))?;

    if request.query.trim().is_empty() {
        return Err("query must not be empty".to_owned());
    }

    Ok(request)
}

#[derive(Debug, serde::Deserialize)]
struct QueryIdRequest {
    query_id: String,
}

fn parse_query_id_request(body: Option<&[u8]>) -> Result<Hash, String> {
    let body = body.ok_or_else(|| "request body is required".to_owned())?;
    let request: QueryIdRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid query id request JSON: {error}"))?;
    types::parse_hash(request.query_id.trim()).map_err(|error| format!("invalid query_id: {error}"))
}

fn search_accepted_response(query_id: Hash) -> http::Response {
    let body = serde_json::json!({
        "status": "submitted",
        "query_id": query_id.to_string(),
    })
    .to_string();
    http_json_response(202, &body)
}

fn search_status_response(
    query_id: Hash,
    completed: bool,
    received_results: usize,
    expected_results: usize,
) -> http::Response {
    let body = serde_json::json!({
        "status": if completed { "completed" } else { "pending" },
        "query_id": query_id.to_string(),
        "completed": completed,
        "received_results": received_results,
        "expected_results": expected_results,
    })
    .to_string();
    http_json_response(200, &body)
}

fn search_pending_response(
    query_id: Hash,
    received_results: usize,
    expected_results: usize,
) -> http::Response {
    let body = serde_json::json!({
        "status": "pending",
        "query_id": query_id.to_string(),
        "completed": false,
        "received_results": received_results,
        "expected_results": expected_results,
        "results": [],
    })
    .to_string();
    http_json_response(409, &body)
}

fn search_results_response(query_id: Hash, results: &[&SearchResult]) -> http::Response {
    let verified: Vec<&SearchResult> = results
        .iter()
        .copied()
        .filter(|result| verify::verify_signed(*result))
        .collect();
    let rejected_results = results.len() - verified.len();
    let aggregated = aggregate_matches(&verified);

    let body = serde_json::json!({
        "status": "completed",
        "query_id": query_id.to_string(),
        "completed": true,
        "verified_results": verified.len(),
        "rejected_results": rejected_results,
        "results": verified,
        "aggregated": aggregated,
    })
    .to_string();
    http_json_response(200, &body)
}

// Deduplicates matches by skill digest, keeping the best score seen for each
// skill, and orders them by descending score then digest.
fn aggregate_matches(results: &[&SearchResult]) -> Vec<SkillMatch> {
    let mut best_scores: HashMap<Hash, Bm25Score> = new_hashmap();
    for result in results {
        for skill_match in &result.matches {
            let score = best_scores
                .entry(skill_match.skill_digest)
                .or_insert(skill_match.bm25_score);
            if skill_match.bm25_score > *score {
                *score = skill_match.bm25_score;
            }
        }
    }

    let mut aggregated: Vec<SkillMatch> = best_scores
        .into_iter()
        .map(|(skill_digest, bm25_score)| SkillMatch {
            skill_digest,
            bm25_score,
        })
        .collect();
    aggregated.sort_by(|left, right| {
        right.bm25_score.cmp(&left.bm25_score).then_with(|| {
            left.skill_digest
                .as_bytes()
                .cmp(right.skill_digest.as_bytes())
        })
    });
    aggregated
}

// Returns the name, description, and digest of every skill registered on this
// node.
#[method::instance(export = http, method = "GET", path_prefix = "/skills/list")]
fn list_skills(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    ensure_skills_loaded(&mut ctx)?;
    let body = match SkillManager::shared() {
        Ok(manager) => skill_list_json(manager),
        Err(SkillError::NotInitialized) => "[]".to_owned(),
        Err(error) => return Ok(http_json_response(500, &json_error(&error.to_string()))),
    };
    Ok(http_json_response(200, &body))
}

fn skill_list_json(manager: &SkillManager) -> String {
    let skills: Vec<_> = manager
        .iter()
        .map(|(digest, skill)| {
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "digest": digest.to_string(),
            })
        })
        .collect();
    serde_json::json!(skills).to_string()
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

fn handle_skill_upload(
    ctx: &mut crate::__lyquid::InstanceContext,
    req: http::Request,
) -> LyquidResult<http::Response> {
    let (skill, digest) = match parse_uploaded_skill(req.body.as_deref()) {
        Ok(skill) => skill,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    if let Err(error) = register_uploaded_skill(&skill) {
        return Ok(http_json_response(500, &json_error(&error.to_string())));
    }

    let digest_hash = match skill.digest() {
        Ok(digest) => digest,
        Err(error) => return Ok(http_json_response(500, &json_error(&error.to_string()))),
    };
    let bytes = match skill.canonical_bytes() {
        Ok(bytes) => bytes,
        Err(error) => return Ok(http_json_response(500, &json_error(&error.to_string()))),
    };
    ctx.instance.skill_blobs.write().insert(digest_hash, bytes);

    println!(
        "mead: accepted skill name={:?} description={:?} digest={digest}",
        skill.name, skill.description
    );

    let body = serde_json::json!({
        "uploaded": 1,
        "digests": [digest],
    })
    .to_string();
    Ok(http_json_response(200, &body))
}

fn parse_uploaded_skill(body: Option<&[u8]>) -> Result<(Skill, String), String> {
    let body = body.ok_or_else(|| "request body is required".to_owned())?;
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| format!("invalid skill JSON: {error}"))?;
    if !value.is_object() {
        return Err("request body must be a JSON object representing one skill".to_owned());
    }

    let skill = match serde_json::from_value::<Skill>(value.clone()) {
        Ok(skill) => skill,
        Err(direct_error) => {
            let encoded = serde_json::to_string(&value)
                .map_err(|error| format!("failed to re-encode skill: {error}"))?;
            Skill::from_json(&encoded).map_err(|error| {
                format!("skill is invalid (serde: {direct_error}; transport: {error})")
            })?
        }
    };

    let digest = skill
        .digest()
        .map_err(|error| format!("skill is invalid: {error}"))?
        .to_string();
    Ok((skill, digest))
}

// Rebuilds the in-memory catalog from the canonical bytes persisted in
// instance state. The process-wide manager does not survive a Lyquid VM
// restart, but instance state does.
fn ensure_skills_loaded(ctx: &mut crate::__lyquid::InstanceContext) -> LyquidResult<()> {
    let blobs: Vec<(Hash, Vec<u8>)> = ctx
        .instance
        .skill_blobs
        .read()
        .iter()
        .map(|(digest, bytes)| (*digest, bytes.clone()))
        .collect();
    if blobs.is_empty() {
        return Ok(());
    }

    if SkillManager::shared().is_err() {
        SkillManager::init_shared(Vec::<Skill>::new())
            .map_err(|error| invalid_input(error.to_string()))?;
    }
    let manager = SkillManager::shared().map_err(|error| invalid_input(error.to_string()))?;
    for (digest, bytes) in blobs {
        if manager.contains(&digest) {
            continue;
        }
        let skill = Skill::from_canonical_bytes(&bytes)
            .map_err(|error| invalid_input(error.to_string()))?;
        manager
            .append(skill)
            .map_err(|error| invalid_input(error.to_string()))?;
    }
    Ok(())
}

fn register_uploaded_skill(skill: &Skill) -> Result<(), SkillError> {
    let manager = match SkillManager::shared() {
        Ok(manager) => Arc::clone(manager),
        Err(SkillError::NotInitialized) => match SkillManager::init_shared(Vec::new()) {
            Ok(manager) => manager,
            Err(SkillError::AlreadyInitialized) => Arc::clone(SkillManager::shared()?),
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    };

    manager.append(skill.clone())
}

fn json_error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn http_json_response(status: u16, body: &str) -> http::Response {
    http::Response {
        status,
        headers: vec![http::Header {
            name: "content-type".to_owned(),
            value: b"application/json".to_vec(),
        }],
        body: body.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_skill(name: &str) -> Skill {
        Skill::new(
            name,
            name,
            format!("{name} description"),
            format!("Use {name}."),
            Vec::new(),
        )
    }

    fn response_json(response: &http::Response) -> serde_json::Value {
        serde_json::from_slice(&response.body).unwrap()
    }

    #[lyquid_test::test]
    fn allocates_a_guest_string() {
        let greeting = String::from("hello");
        assert_eq!(greeting.as_str(), "hello");
    }

    #[lyquid_test::test]
    fn parses_a_single_direct_serde_skill() {
        let skill = Skill::new(
            "direct-serde",
            "direct-serde",
            "Direct serde description.",
            "Use direct serde.",
            vec![crate::skill::SkillResource::new("nested/a.txt", "contents").unwrap()],
        );
        let body = serde_json::to_vec(&skill).unwrap();

        let (parsed, digest) = parse_uploaded_skill(Some(&body)).unwrap();

        assert_eq!(parsed, skill);
        assert_eq!(digest, skill.digest().unwrap().to_string());
    }

    #[lyquid_test::test]
    fn parses_a_single_transport_skill() {
        let skill = test_skill("transport-serde");
        let body = skill.to_json().unwrap();

        let (parsed, digest) = parse_uploaded_skill(Some(body.as_bytes())).unwrap();

        assert_eq!(parsed, skill);
        assert_eq!(digest, skill.digest().unwrap().to_string());
    }

    #[lyquid_test::test]
    fn rejects_skill_arrays_and_non_objects() {
        let skills = vec![test_skill("one"), test_skill("two")];
        let array = serde_json::to_vec(&skills).unwrap();
        let error = parse_uploaded_skill(Some(&array)).unwrap_err();
        assert!(error.contains("JSON object"));

        let error = parse_uploaded_skill(Some(b"null")).unwrap_err();
        assert!(error.contains("JSON object"));
    }

    #[lyquid_test::test]
    fn rejects_missing_and_malformed_request_bodies() {
        let error = parse_uploaded_skill(None).unwrap_err();
        assert!(error.contains("required"));

        let error = parse_uploaded_skill(Some(b"{")).unwrap_err();
        assert!(error.contains("invalid skill JSON"));
    }

    #[lyquid_test::test]
    fn rejects_invalid_transport_skills() {
        let body = serde_json::json!({
            "name": "invalid",
            "description": "Invalid skill.",
            "instructions": "Use it.",
            "resources": [{"path": "../escape", "contents": "nope"}],
        })
        .to_string();

        let error = parse_uploaded_skill(Some(body.as_bytes())).unwrap_err();

        assert!(error.contains("skill is invalid"));
    }

    #[lyquid_test::test]
    fn parses_a_search_request_with_a_default_k() {
        let request = parse_search_endpoint_request(Some(br#"{"query":"rust async"}"#)).unwrap();

        assert_eq!(request.query, "rust async");
        assert_eq!(request.k, 10);
    }

    #[lyquid_test::test]
    fn parses_a_search_request_with_an_explicit_k() {
        let request =
            parse_search_endpoint_request(Some(br#"{"query":"rust async","k":5}"#)).unwrap();

        assert_eq!(request.query, "rust async");
        assert_eq!(request.k, 5);
    }

    #[lyquid_test::test]
    fn rejects_missing_empty_and_malformed_search_requests() {
        let error = parse_search_endpoint_request(None).unwrap_err();
        assert!(error.contains("required"));

        let error = parse_search_endpoint_request(Some(br#"{"query":"   "}"#)).unwrap_err();
        assert!(error.contains("must not be empty"));

        let error = parse_search_endpoint_request(Some(b"{")).unwrap_err();
        assert!(error.contains("invalid search request JSON"));
    }

    #[lyquid_test::test]
    fn query_text_round_trips_through_certified_input_encoding() {
        let query =
            SearchQuery::new(b"skill", QueryParams { k: 5 }, 1, Address::from([1u8; 20])).unwrap();
        let encoded = encode_by_fields!(query: SearchQuery = query.clone());
        let decoded = decode_by_fields!(&encoded, query: SearchQuery).unwrap();

        assert_eq!(decoded.query.query_text, "skill");
        assert_eq!(decoded.query, query);
    }

    #[lyquid_test::test]
    fn search_accepted_response_contains_the_query_id() {
        let query_id = blake3::hash(b"query");
        let response = search_accepted_response(query_id);
        let body = response_json(&response);

        assert_eq!(response.status, 202);
        assert_eq!(body["status"].as_str(), Some("submitted"));
        assert_eq!(
            body["query_id"].as_str(),
            Some(query_id.to_string().as_str())
        );
    }

    #[lyquid_test::test]
    fn parses_query_ids_from_json_bodies() {
        let query_id = blake3::hash(b"query");

        let body = format!(r#"{{"query_id":"{query_id}"}}"#);
        assert_eq!(
            parse_query_id_request(Some(body.as_bytes())).unwrap(),
            query_id
        );

        let body = format!(r#"{{"query_id":"0x{query_id}"}}"#);
        assert_eq!(
            parse_query_id_request(Some(body.as_bytes())).unwrap(),
            query_id
        );
    }

    #[lyquid_test::test]
    fn rejects_missing_and_malformed_query_ids() {
        let error = parse_query_id_request(None).unwrap_err();
        assert!(error.contains("required"));

        let error = parse_query_id_request(Some(br#"{"query_id":"0x1234"}"#)).unwrap_err();
        assert!(error.contains("invalid query_id"));

        let error = parse_query_id_request(Some(b"{")).unwrap_err();
        assert!(error.contains("invalid query id request JSON"));
    }

    #[lyquid_test::test]
    fn search_status_response_reports_progress() {
        let query_id = blake3::hash(b"query");
        let response = search_status_response(query_id, false, 2, 4);
        let body = response_json(&response);

        assert_eq!(response.status, 200);
        assert_eq!(body["status"].as_str(), Some("pending"));
        assert_eq!(body["completed"].as_bool(), Some(false));
        assert_eq!(body["received_results"].as_u64(), Some(2));
        assert_eq!(body["expected_results"].as_u64(), Some(4));
    }

    #[lyquid_test::test]
    fn search_pending_response_returns_409() {
        let query_id = blake3::hash(b"query");
        let response = search_pending_response(query_id, 1, 3);
        let body = response_json(&response);

        assert_eq!(response.status, 409);
        assert_eq!(body["completed"].as_bool(), Some(false));
        assert_eq!(body["received_results"].as_u64(), Some(1));
        assert_eq!(body["expected_results"].as_u64(), Some(3));
        assert_eq!(body["results"].as_array().map(Vec::len), Some(0));
    }

    #[lyquid_test::test]
    fn search_results_response_contains_results() {
        let query_id = blake3::hash(b"query");
        let response = search_results_response(query_id, &[]);
        let body = response_json(&response);

        assert_eq!(response.status, 200);
        assert_eq!(body["completed"].as_bool(), Some(true));
        assert_eq!(body["results"].as_array().map(Vec::len), Some(0));
        assert_eq!(body["aggregated"].as_array().map(Vec::len), Some(0));
        assert_eq!(body["verified_results"].as_u64(), Some(0));
        assert_eq!(body["rejected_results"].as_u64(), Some(0));
    }

    #[lyquid_test::test]
    fn aggregate_matches_keeps_best_score_and_sorts() {
        let query_id = blake3::hash(b"query");
        let digest_a = blake3::hash(b"skill-a");
        let digest_b = blake3::hash(b"skill-b");
        let digest_c = blake3::hash(b"skill-c");
        let first = SearchResult::new(
            query_id,
            vec![
                SkillMatch {
                    skill_digest: digest_a,
                    bm25_score: 2,
                },
                SkillMatch {
                    skill_digest: digest_c,
                    bm25_score: 4,
                },
            ],
            1,
            Address::from([1u8; 20]),
        )
        .unwrap();
        let second = SearchResult::new(
            query_id,
            vec![
                SkillMatch {
                    skill_digest: digest_a,
                    bm25_score: 9,
                },
                SkillMatch {
                    skill_digest: digest_b,
                    bm25_score: 5,
                },
            ],
            2,
            Address::from([2u8; 20]),
        )
        .unwrap();

        let aggregated = aggregate_matches(&[&first, &second]);
        assert_eq!(aggregated.len(), 3);
        assert_eq!(aggregated[0].skill_digest, digest_a);
        assert_eq!(aggregated[0].bm25_score, 9);
        assert_eq!(aggregated[1].skill_digest, digest_b);
        assert_eq!(aggregated[1].bm25_score, 5);
        assert_eq!(aggregated[2].skill_digest, digest_c);
        assert_eq!(aggregated[2].bm25_score, 4);
    }

    #[lyquid_test::test]
    fn count_response_reflects_manager_contents() {
        let manager = SkillManager::from_skills([test_skill("counted")]).unwrap();
        let response = skill_count_http_response(manager.len());
        let body = response_json(&response);

        assert_eq!(response.status, 200);
        assert_eq!(body["count"].as_u64(), Some(1));
    }

    #[lyquid_test::test]
    fn list_response_contains_name_description_and_digest() {
        let skill = test_skill("listed");
        let digest = skill.digest().unwrap().to_string();
        let manager = SkillManager::from_skills([skill.clone()]).unwrap();

        let body: serde_json::Value = serde_json::from_str(&skill_list_json(&manager)).unwrap();
        let entries = body.as_array().unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"].as_str(), Some(skill.name.as_str()));
        assert_eq!(
            entries[0]["description"].as_str(),
            Some(skill.description.as_str())
        );
        assert_eq!(entries[0]["digest"].as_str(), Some(digest.as_str()));
        assert_eq!(entries[0].as_object().unwrap().len(), 3);
    }
}
