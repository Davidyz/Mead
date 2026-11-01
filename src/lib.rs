//! Mead: blockchain-based P2P skill distribution platform.
//!
//! On-chain state consists of a skill directory and query/result registries
//! that track search queries and peer-submitted results.

use lyquid::prelude::lyquor_primitives::HashBytes;
use lyquid::prelude::*;
use lyquid::println;
use std::sync::Arc;

pub mod skill;
pub mod types;
pub mod verify;

use skill::{Skill, SkillError, SkillManager};
use types::invalid_input;
use types::{
    NodeRegistry, QueryRegistry, ResultRegistry, SEARCH_RESPONSE_DEADLINE_BLOCKS, SearchQuery,
    SearchResult,
};

/// Maximum time the initiating node waits for a UPC acknowledgement.
const UPC_ANNOUNCE_TIMEOUT_MS: u64 = 10_000;

state! {
    // On-chain registries
    network query_registry: QueryRegistry = QueryRegistry::default();
    network result_registry: ResultRegistry = ResultRegistry::default();
    network node_registry: NodeRegistry = NodeRegistry::default();

    // Node-local notifications
    instance pending_searches: Vec<Hash> = Vec::new();
    instance ready_queries: Vec<Hash> = Vec::new();
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

    let query_id = query.query_id;
    let initiator = lyquor_api::get_ed25519_by_address(query.sender)
        .ok()
        .flatten()
        .ok_or_else(|| invalid_input("query sender has no ed25519 identity"))?;

    ctx.network.query_registry.insert(query)?;

    // Snapshot the peers at registration time, excluding the initiating node.
    let mut expected_nodes: Vec<NodeID> = ctx
        .network
        .node_registry
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

    ctx.network.query_registry.plan(
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

    let query = ctx
        .network
        .query_registry
        .get(&result.query_id)
        .cloned()
        .ok_or_else(|| LyquidError::LyquidRuntime("query is not registered".into()))?;

    let query_id = result.query_id;
    let sender = result.sender;

    ctx.network.result_registry.insert(&query, result)?;
    ctx.network
        .query_registry
        .mark_responder(query_id, sender)?;

    let position = lyquor_api::chain_pos()?;
    if ctx.network.query_registry.is_complete(&query_id, position) {
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

    let input = encode_by_fields!(query_id: HashBytes = HashBytes::from(query_id));
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

// UPC request step: runs on each selected peer and records the pending query.
#[method::instance(upc(request))]
fn search_request(ctx: &mut _, query_id: HashBytes) -> LyquidResult<bool> {
    let query_id = Hash::from(query_id);

    if !ctx.network.query_registry.contains(&query_id) {
        return Err(invalid_input("query is not registered"));
    }

    let mut pending = ctx.instance.pending_searches.write();
    if !pending.contains(&query_id) {
        pending.push(query_id);
    }
    Ok(true)
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
fn get_skill_count(_ctx: &_, _req: http::Request) -> LyquidResult<http::Response> {
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

// Returns the name, description, and digest of every skill registered on this
// node.
#[method::instance(export = http, method = "GET", path_prefix = "/skills/list")]
fn list_skills(_ctx: &_, _req: http::Request) -> LyquidResult<http::Response> {
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
fn upload_skills_at_skills_upload(_ctx: &_, req: http::Request) -> LyquidResult<http::Response> {
    handle_skill_upload(req)
}

fn handle_skill_upload(req: http::Request) -> LyquidResult<http::Response> {
    let (skill, digest) = match parse_uploaded_skill(req.body.as_deref()) {
        Ok(skill) => skill,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    if let Err(error) = register_uploaded_skill(&skill) {
        return Ok(http_json_response(500, &json_error(&error.to_string())));
    }

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
