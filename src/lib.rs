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
    handle_skill_get, handle_skill_upload, skill_count_response, skill_list_json,
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
    BPS_DENOMINATOR, Bm25Score, BondRegistry, CanonicalBytes, NodeRegistry, QueryParams,
    QueryRegistry, ResultRegistry, SEARCH_RESPONSE_DEADLINE_BLOCKS, SearchQuery, SearchResult,
};

/// Maximum time the initiating node waits for a UPC acknowledgement.
const UPC_ANNOUNCE_TIMEOUT_MS: u64 = 10_000;

/// Automatic slash applied to a signed result that fails on-chain validation.
const SLASH_INVALID_RESULT_BPS: u16 = BPS_DENOMINATOR;

mod bm25;

pub use bm25::search;

state! {
    // Oracle topic used to certify search initiation calls.
    network oracle search;

    // On-chain registries
    network query_registry: QueryRegistry = QueryRegistry::default();
    network result_registry: ResultRegistry = ResultRegistry::default();
    network node_registry: NodeRegistry = NodeRegistry::default();
    // Bond accounting for the peer penalty mechanism. The zero LyquidID means
    // bonds are disabled; `min_bond` is then ignored.
    network bonds: BondRegistry = BondRegistry::default();
    network token_lyquid_id: LyquidID = LyquidID::default();
    network min_bond: U256 = U256::ZERO;
    // Canonical skill bytes persisted in instance state so the in-memory
    // manager can be rebuilt after the Lyquid VM restarts.
    instance skill_blobs: HashMap<Hash, Vec<u8>> = new_hashmap();
    // Peer-provided skill bytes persisted for `/skills/get`.
    // TODO: bound this map. There is no direct fetch-by-digest path to another
    // node yet, so evicting an entry would make a skill whose digest the caller
    // still holds unavailable until another search returns it. Tracked in #27.
    instance acquired_skill_blobs: HashMap<Hash, Vec<u8>> = new_hashmap();
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
fn constructor(
    ctx: &mut _,
    committee: Vec<NodeID>,
    threshold: u16,
    token_lyquid_id: LyquidID,
    min_bond: U256,
) {
    *ctx.network.token_lyquid_id = token_lyquid_id;
    *ctx.network.min_bond = min_bond;
    if let Ok(seq_id) = lyquor_api::sequence_backend_id() {
        let target = OracleTarget {
            seq_id,
            target: OracleServiceTarget::LVM(ctx.lyquid_id),
        };
        let oracle = ctx.network.search.clone();
        let _ = oracle.initialize(&mut ctx, target, committee, threshold);
    }
}

/// Builds the oracle target for the `search` topic on this Lyquid.
fn search_oracle_target(lyquid_id: LyquidID) -> LyquidResult<OracleTarget> {
    Ok(OracleTarget {
        seq_id: lyquor_api::sequence_backend_id()?,
        target: OracleServiceTarget::LVM(lyquid_id),
    })
}

/// Returns the configured bond token or an error when bonds are disabled.
fn bond_token(token: LyquidID) -> LyquidResult<LyquidID> {
    if token == LyquidID::default() {
        return Err(invalid_input("the bond token is not configured"));
    }
    Ok(token)
}

/// Resolves a node identity to its submitter address for token transfers.
fn bond_account(node: NodeID) -> LyquidResult<Address> {
    lyquor_api::get_address_by_ed25519(node.0)
        .map_err(|error| LyquidError::LyquidRuntime(error.to_string()))?
        .ok_or_else(|| invalid_input("the node has no associated account"))
}

fn safe_transfer_from(
    token: LyquidID,
    from: Address,
    to: Address,
    amount: U256,
) -> LyquidResult<()> {
    let success = call!(
        (token).transferFrom(from: Address = from, to: Address = to, value: U256 = amount)
            -> (success: LyquidResult<bool>)
    )
    .success?;
    if !success {
        return Err(invalid_input("bond token transferFrom failed"));
    }
    Ok(())
}

fn safe_transfer(token: LyquidID, to: Address, amount: U256) -> LyquidResult<()> {
    let success = call!(
        (token).transfer(to: Address = to, amount: U256 = amount)
            -> (success: LyquidResult<bool>)
    )
    .success?;
    if !success {
        return Err(invalid_input("bond token transfer failed"));
    }
    Ok(())
}

/// Returns the search oracle's current destination epoch (0 when not active).
fn search_oracle_epoch(target: OracleTarget) -> LyquidResult<u32> {
    Ok(
        lyquor_api::fetch_oracle_info("search".to_owned(), target, false)?
            .map(|info| info.epoch)
            .unwrap_or(0),
    )
}

fn oracle_activation_response(status: u16, state: &str, epoch: u32) -> http::Response {
    let body = serde_json::json!({
        "status": state,
        "epoch": epoch,
    })
    .to_string();
    http_json_response(status, &body)
}

/// Submits a finalize certificate for the search oracle epoch.
///
/// `finalize_epoch` always builds a certificate while the source config is
/// valid, so its return value cannot tell us whether finalization is still
/// pending. Callers should gate on the source epoch instead (see
/// `StateVar::get_epoch`). Submission errors are propagated so the endpoint can
/// report a failed finalize instead of returning `202` forever.
fn submit_search_oracle_finalize(
    ctx: &mut crate::__lyquid::InstanceContext,
    target: OracleTarget,
) -> LyquidResult<()> {
    let oracle = ctx.network.search.clone();
    if let Some(call) = oracle.finalize_epoch(ctx, target)? {
        submit_certified_call!(call)?;
    }
    Ok(())
}

// Activates the search oracle's first epoch.
//
// `POST /oracle/activate`
//
// The constructor only stages the committee; the topic cannot certify calls
// until the first epoch advance/finalize round settles. This endpoint performs
// that round. It is idempotent: an already-active oracle returns 200
// immediately and submits no calls.
//
// The certified epoch calls execute asynchronously, so this returns `202` until
// they settle. Poll this endpoint (or `GET /oracle/status`) until `200`; each
// call submits only the work that is still needed.
//
// Response - `200 OK` (already active or fully active by the time the call
// returned):
// { "status": "active", "epoch": 1 }
//
// Response - `202 Accepted` (activation in progress; retry):
// { "status": "activating", "epoch": 0 }
//
// Errors:
// - `502`: submitting the certified epoch advance or finalize failed.
// - `503`: the oracle is not staged.
#[method::instance(export = http, method = "POST", path_prefix = "/oracle/activate")]
fn activate_search_oracle(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    let target = search_oracle_target(ctx.lyquid_id)?;
    let oracle = ctx.network.search.clone();

    let mut epoch = search_oracle_epoch(target)?;
    if epoch == 0 {
        let Some(call) = oracle.advance_epoch(&mut ctx, target)? else {
            return Ok(oracle_activation_response(503, "not_staged", 0));
        };
        if let Err(error) = submit_certified_call!(call) {
            return Ok(http_json_response(
                502,
                &json_error(&format!(
                    "failed to submit the search oracle epoch advance: {error}"
                )),
            ));
        }
        epoch = search_oracle_epoch(target)?;
    }

    if epoch > 0 {
        // Certification needs the source-side config at the *same* epoch. A
        // non-empty committee from an older epoch is not enough if the
        // destination advanced again.
        let source_active = ctx.network.search.get_epoch(&ctx, target) == epoch;
        if source_active {
            return Ok(oracle_activation_response(200, "active", epoch));
        }
        if let Err(error) = submit_search_oracle_finalize(&mut ctx, target) {
            return Ok(http_json_response(
                502,
                &json_error(&format!(
                    "failed to submit the search oracle epoch finalize: {error}"
                )),
            ));
        }
    }

    Ok(oracle_activation_response(202, "activating", epoch))
}

// Reports whether the search oracle is active.
//
// `GET /oracle/status`
//
// `active` is true when the destination epoch is non-zero and the source's
// finalized epoch matches it. `source_active` reports that match and
// `source_epoch` reports the source's finalized epoch.
//
// Response - `200 OK`:
// { "active": false, "epoch": 0, "source_active": false, "source_epoch": 0 }
#[method::instance(export = http, method = "GET", path_prefix = "/oracle/status")]
fn search_oracle_status(ctx: &_, _req: http::Request) -> LyquidResult<http::Response> {
    let target = search_oracle_target(ctx.lyquid_id)?;
    let epoch = search_oracle_epoch(target)?;
    let source_epoch = ctx.network.search.get_epoch(&ctx, target);
    let source_active = epoch > 0 && source_epoch == epoch;
    let body = serde_json::json!({
        "active": source_active,
        "epoch": epoch,
        "source_active": source_active,
        "source_epoch": source_epoch,
    })
    .to_string();
    Ok(http_json_response(200, &body))
}

// Registers a node identity through a certified oracle call.
//
// The node's local `/nodes/register` endpoint obtains a certificate for this
// call using its own `ctx.node_id`; the committee attests the request and no
// address-to-key mapping is involved. Registration is idempotent.
#[method::network(group = oracle::certified::search)]
fn register_node(ctx: &mut _, node: NodeID) -> LyquidResult<()> {
    // When bonds are enabled, a node must meet the minimum before it can serve
    // searches. A zero token LyquidID keeps bonds disabled for existing
    // deployments that have not deployed a token yet.
    if *ctx.network.token_lyquid_id != LyquidID::default()
        && !ctx.network.bonds.is_eligible(node, *ctx.network.min_bond)
    {
        return Err(invalid_input("node bond is below the minimum"));
    }

    ctx.network.node_registry.register(node)?;
    Ok(())
}

// Removes a node identity through a certified oracle call.
//
// The node's local `/nodes/unregister` endpoint supplies its own `ctx.node_id`.
#[method::network(group = oracle::certified::search)]
fn unregister_node(ctx: &mut _, node: NodeID) -> LyquidResult<()> {
    if !ctx.network.node_registry.contains(&node) {
        return Err(invalid_input("node is not registered"));
    }

    // Return any remaining bond before removing the node. Slashing removes the
    // node directly instead of going through this path, so slashed funds stay
    // in Mead's treasury.
    let amount = ctx.network.bonds.amount(node);
    if !amount.is_zero() {
        let token = bond_token(*ctx.network.token_lyquid_id)?;
        let account = bond_account(node)?;
        safe_transfer(token, account, amount)?;
        ctx.network.bonds.withdraw(node);
    }

    ctx.network.node_registry.unregister(node);
    Ok(())
}

// Posts a token bond for a node through a certified oracle call.
//
// The node's local `/bonds/post` endpoint supplies its own `ctx.node_id`; the
// transfer pulls tokens from the node's account into Mead, which acts as the
// bond treasury. The node must approve Mead on the token first.
#[method::network(group = oracle::certified::search)]
fn post_bond(ctx: &mut _, node: NodeID, amount: U256) -> LyquidResult<()> {
    if node.0 == [0u8; 32] {
        return Err(invalid_input("node must not be zero"));
    }
    if amount.is_zero() {
        return Err(invalid_input("bond amount must be greater than zero"));
    }

    let token = bond_token(*ctx.network.token_lyquid_id)?;
    let account = bond_account(node)?;
    let treasury = Address::from(ctx.lyquid_id);
    safe_transfer_from(token, account, treasury, amount)?;

    let posted_at = lyquor_api::systime()? / 1000;
    ctx.network.bonds.post(node, amount, posted_at)
}

// Withdraws a node's remaining bond through a certified oracle call.
#[method::network(group = oracle::certified::search)]
fn withdraw_bond(ctx: &mut _, node: NodeID) -> LyquidResult<()> {
    let amount = ctx.network.bonds.amount(node);
    if amount.is_zero() {
        return Ok(());
    }

    let token = bond_token(*ctx.network.token_lyquid_id)?;
    let account = bond_account(node)?;
    safe_transfer(token, account, amount)?;
    ctx.network.bonds.withdraw(node);
    Ok(())
}

// Slashes a node's bond for a signed but invalid search result.
//
// The oracle policy only certifies this call when `result` carries a valid
// signature by `result.sender` and fails result validation, so a node cannot be
// slashed for a valid result or for a result signed by someone else.
#[method::network(group = oracle::certified::search)]
fn slash_bond(ctx: &mut _, result: SearchResult, fraction_bps: u16) -> LyquidResult<()> {
    if !verify::verify_signed(&result) {
        return Err(invalid_input("slash evidence signature is invalid"));
    }
    if !ctx.network.node_registry.contains(&result.sender) {
        return Err(invalid_input("slash target is not registered"));
    }

    let query = ctx
        .network
        .query_registry
        .get(&result.query_id)
        .cloned()
        .ok_or_else(|| invalid_input("slash evidence query is not registered"))?;
    if result.validate().is_ok() && result.validate_against(&query).is_ok() {
        return Err(invalid_input("slash evidence is a valid result"));
    }

    let slashed = ctx.network.bonds.slash(result.sender, fraction_bps)?;
    lyquid::println!(
        "mead: slashed {slashed} from {} for query {}",
        result.sender,
        result.query_id
    );

    let remaining = ctx.network.bonds.amount(result.sender);
    if remaining.is_zero()
        || !ctx
            .network
            .bonds
            .is_eligible(result.sender, *ctx.network.min_bond)
    {
        ctx.network.node_registry.unregister(result.sender);
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
// `POST /nodes/register`
//
// No request body is required. If the node is already registered, returns `200`
// immediately without certifying anything. Otherwise the endpoint certifies and
// submits `register_node(ctx.node_id)` for the node hosting this endpoint and
// returns `202`. It requires an active search oracle.
//
// Response - `200 OK` (already registered):
// {
//   "status": "registered",
//   "method": "register_node",
//   "node_id": "Node-..."
// }
//
// Response - `202 Accepted` (registration submitted):
// {
//   "status": "submitted",
//   "method": "register_node",
//   "node_id": "Node-..."
// }
//
// Errors:
// - `502`: submitting the certified call failed.
// - `503`: search certification is unavailable.
#[method::instance(export = http, method = "POST", path_prefix = "/nodes/register")]
fn register_node_endpoint(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    let node = ctx.node_id;

    // Cheap idempotency fast path: a restarted MCP or a repeated bootstrap call
    // should not trigger a committee certification round and a sequenced
    // transaction when the node is already in the registry.
    if ctx.network.node_registry.contains(&node) {
        let body = serde_json::json!({
            "status": "registered",
            "method": "register_node",
            "node_id": node.to_string(),
        })
        .to_string();
        return Ok(http_json_response(200, &body));
    }

    submit_node_registry_change(&mut ctx, "register_node", node)
}

// Removes this node from Mead's node registry.
//
// `POST /nodes/unregister`
//
// No request body is required. The endpoint certifies and submits
// `unregister_node(ctx.node_id)` for the node hosting this endpoint.
//
// Response - `202 Accepted`:
// {
//   "status": "submitted",
//   "method": "unregister_node",
//   "node_id": "Node-..."
// }
//
// Errors:
// - `502`: submitting the certified call failed.
// - `503`: search certification is unavailable.
#[method::instance(export = http, method = "POST", path_prefix = "/nodes/unregister")]
fn unregister_node_endpoint(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    let node = ctx.node_id;
    submit_node_registry_change(&mut ctx, "unregister_node", node)
}

// Lists the node identities currently registered on-chain.
//
// `GET /nodes/list`
//
// Response - `200 OK`:
// {
//   "nodes": ["Node-...", "Node-..."]
// }
#[method::instance(export = http, method = "GET", path_prefix = "/nodes/list")]
fn list_registered_nodes(ctx: &_, _req: http::Request) -> LyquidResult<http::Response> {
    let mut nodes: Vec<NodeID> = ctx.network.node_registry.nodes().iter().copied().collect();
    nodes.sort_by_key(|node| node.0);
    let nodes: Vec<String> = nodes.into_iter().map(|node| node.to_string()).collect();
    let body = serde_json::json!({ "nodes": nodes }).to_string();
    Ok(http_json_response(200, &body))
}

#[derive(serde::Deserialize)]
struct PostBondRequest {
    amount: String,
}

fn parse_bond_amount(value: &str) -> LyquidResult<U256> {
    let value = value.trim();
    let (digits, radix) = match value.strip_prefix("0x") {
        Some(hex) => (hex, 16),
        None => (value, 10),
    };
    U256::from_str_radix(digits, radix)
        .map_err(|_| invalid_input("bond amount must be a decimal or 0x-prefixed integer"))
}

fn submit_certified_bond_change(
    ctx: &mut crate::__lyquid::InstanceContext,
    method: &str,
    input: Vec<u8>,
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
            input: input.into(),
            target,
        },
        Bytes::new(),
        None,
        Some(UPC_ANNOUNCE_TIMEOUT_MS),
    )?;

    let Some(call) = call else {
        return Ok(http_json_response(
            503,
            &json_error("bond certification is unavailable"),
        ));
    };

    match submit_certified_call!(call) {
        Ok(_) => {
            let body = serde_json::json!({
                "status": "submitted",
                "method": method,
            })
            .to_string();
            Ok(http_json_response(202, &body))
        }
        Err(error) => Ok(http_json_response(
            502,
            &json_error(&format!("failed to submit certified bond change: {error}")),
        )),
    }
}

// Posts a token bond for this node.
//
// `POST /bonds/post`
//
// Request body:
// { "amount": "1000" }
//
// The amount is a decimal integer or a `0x`-prefixed hex integer. The node must
// approve Mead on the bond token before posting. Requires an active search
// oracle and a configured bond token.
//
// Response - `202 Accepted`:
// { "status": "submitted", "method": "post_bond" }
//
// Errors:
// - `400`: the amount is missing, empty, or invalid.
// - `502`: submitting the certified call failed.
// - `503`: bond certification is unavailable.
#[method::instance(export = http, method = "POST", path_prefix = "/bonds/post")]
fn post_bond_endpoint(ctx: &mut _, req: http::Request) -> LyquidResult<http::Response> {
    let Some(body) = req.body.as_deref() else {
        return Ok(http_json_response(
            400,
            &json_error("request body is required"),
        ));
    };
    let request: PostBondRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(error) => {
            return Ok(http_json_response(
                400,
                &json_error(&format!("invalid bond request JSON: {error}")),
            ));
        }
    };
    let amount = match parse_bond_amount(&request.amount) {
        Ok(amount) => amount,
        Err(error) => return Ok(http_json_response(400, &json_error(&error.to_string()))),
    };
    if amount.is_zero() {
        return Ok(http_json_response(
            400,
            &json_error("bond amount must be greater than zero"),
        ));
    }

    let node = ctx.node_id;
    let input = encode_by_fields!(node: NodeID = node, amount: U256 = amount).into();
    submit_certified_bond_change(&mut ctx, "post_bond", input)
}

// Withdraws this node's remaining bond.
//
// `POST /bonds/withdraw`
//
// Response - `202 Accepted`:
// { "status": "submitted", "method": "withdraw_bond" }
//
// Errors:
// - `502`: submitting the certified call failed.
// - `503`: bond certification is unavailable.
#[method::instance(export = http, method = "POST", path_prefix = "/bonds/withdraw")]
fn withdraw_bond_endpoint(ctx: &mut _, _req: http::Request) -> LyquidResult<http::Response> {
    let node = ctx.node_id;
    let input = encode_by_fields!(node: NodeID = node).into();
    submit_certified_bond_change(&mut ctx, "withdraw_bond", input)
}

// Reports this node's bond state.
//
// `GET /bonds/status`
//
// Response - `200 OK`:
// { "node_id": "Node-...", "bond": "1000", "min_bond": "1000",
//   "eligible": true, "token_lyquid_id": "Lyquid-..." }
#[method::instance(export = http, method = "GET", path_prefix = "/bonds/status")]
fn bond_status_endpoint(ctx: &_, _req: http::Request) -> LyquidResult<http::Response> {
    let node = ctx.node_id;
    let bond = ctx.network.bonds.amount(node);
    let min_bond = *ctx.network.min_bond;
    let token = *ctx.network.token_lyquid_id;
    let body = serde_json::json!({
        "node_id": node.to_string(),
        "bond": bond.to_string(),
        "min_bond": min_bond.to_string(),
        "eligible": ctx.network.bonds.is_eligible(node, min_bond),
        "token_lyquid_id": token.to_string(),
    })
    .to_string();
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

// Stores a verified result submitted by an expected peer.
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

    // Nothing to wait for: completion is derived from the registry state.
    if expected_nodes.is_empty() {
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

    let mut skill_bodies = Vec::with_capacity(result.matches.len());
    if !result.matches.is_empty() {
        let manager = SkillManager::shared().map_err(|error| invalid_input(error.to_string()))?;
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

// Certifies and submits a slash for a signed-but-invalid peer result.
fn submit_slash_impl(
    ctx: &mut crate::__lyquid::InstanceContext,
    result: &SearchResult,
    fraction_bps: u16,
) -> LyquidResult<()> {
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
            method: "slash_bond".into(),
            input: encode_by_fields!(
                result: SearchResult = result.clone(),
                fraction_bps: u16 = fraction_bps
            )
            .into(),
            target,
        },
        Bytes::new(),
        None,
        Some(UPC_ANNOUNCE_TIMEOUT_MS),
    )?;

    let Some(call) = call else {
        return Err(invalid_input("slash certification is unavailable"));
    };
    submit_certified_call!(call)?;
    Ok(())
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
    if let Err(error) = result.validate_against(&query) {
        // The signature is valid, so a self-inconsistent or query-inconsistent
        // result is attributable to the sender. Slash it before returning the
        // original validation error.
        if let Err(slash_error) = submit_slash_impl(ctx, &result, SLASH_INVALID_RESULT_BPS) {
            lyquid::println!(
                "mead: failed to slash {} for invalid result: {slash_error}",
                result.sender
            );
        }
        return Err(error);
    }
    if skill_bodies.len() != result.matches.len() {
        return Err(invalid_input(
            "result does not include a body for every matched skill",
        ));
    }

    let mut matched_skills = Vec::with_capacity(result.matches.len());
    let mut matched_digests = Vec::with_capacity(result.matches.len());
    let mut matched_blobs = Vec::with_capacity(result.matches.len());
    for (skill_match, body) in result.matches.iter().zip(skill_bodies.iter()) {
        let skill =
            Skill::from_canonical_bytes(body).map_err(|error| invalid_input(error.to_string()))?;
        let digest = skill
            .digest()
            .map_err(|error| invalid_input(error.to_string()))?;
        if digest != skill_match.skill_digest {
            return Err(invalid_input("skill body does not match the match digest"));
        }
        matched_digests.push(digest);
        matched_blobs.push(body.clone());
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

    // Persist peer bodies only after every verification step passes, so
    // rejected replies cannot populate `/skills/get`. They stay out of the
    // local BM25 corpus.
    {
        let mut blobs = ctx.instance.acquired_skill_blobs.write();
        for (digest, body) in matched_digests.iter().zip(matched_blobs.iter()) {
            blobs.insert(*digest, body.clone());
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
        "register_node" | "unregister_node" | "withdraw_bond" => {
            let Some(input) = decode_by_fields!(&params.input, node: NodeID) else {
                return Ok(false);
            };
            Ok(input.node.0 != [0u8; 32])
        }
        "post_bond" => {
            let Some(input) = decode_by_fields!(&params.input, node: NodeID, amount: U256) else {
                return Ok(false);
            };
            Ok(input.node.0 != [0u8; 32] && !input.amount.is_zero())
        }
        "slash_bond" => {
            let Some(input) = decode_by_fields!(
                &params.input,
                result: SearchResult,
                fraction_bps: u16
            ) else {
                return Ok(false);
            };
            if !verify::verify_signed(&input.result)
                || input.fraction_bps == 0
                || input.fraction_bps > BPS_DENOMINATOR
                || !ctx.network.node_registry.contains(&input.result.sender)
            {
                return Ok(false);
            }
            let Some(query) = ctx.network.query_registry.get(&input.result.query_id) else {
                return Ok(false);
            };
            Ok(input.result.validate().is_err() || input.result.validate_against(query).is_err())
        }
        _ => Ok(false),
    }
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
    responders.sort_by_key(|node| node.0);
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
// `POST /search/new`
//
// The endpoint builds and signs a `SearchQuery` from the node's own identity,
// then submits a certified call to the search oracle. The certified network
// method registers the query and triggers the peer-coordination UPC.
//
// Request:
// ```json
// { "query": "rust async", "k": 5 }
// ```
// - `query` (string, required): search keywords.
// - `k` (number, optional): results per peer. Defaults to `10`. Must be within
//   the supported bounds.
//
// Response - `202 Accepted`:
// ```json
// {
//   "status": "submitted",
//   "query_id": "8c62da8117dd88322d28cb4fb59b561007e3109476938e071732fee5f25bf399"
// }
// ```
//
// `202` means the certified `coordinate_search` call was submitted, not that the
// query is registered yet. Poll `POST /search/status` until it returns `200`
// before expecting peers to answer.
//
// Errors:
// - `400`: missing/invalid JSON, empty query, or invalid `k`.
// - `500`: certification failed unexpectedly.
// - `502`: submitting the certified call failed.
// - `503`: the search oracle is not active; call `POST /oracle/activate` and
//   retry.
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

    let target = search_oracle_target(ctx.lyquid_id)?;
    if search_oracle_epoch(target)? == 0 {
        return Ok(http_json_response(
            503,
            &json_error("search oracle is not active; call POST /oracle/activate and retry"),
        ));
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
//
// `POST /search/status`
//
// Request:
// ```json
// { "query_id": "8c62da8117dd88322d28cb4fb59b561007e3109476938e071732fee5f25bf399" }
// ```
//
// Response - `200 OK`:
// ```json
// {
//   "status": "pending",
//   "query_id": "8c62da8117dd88322d28cb4fb59b561007e3109476938e071732fee5f25bf399",
//   "completed": false,
//   "timed_out": false,
//   "received_results": 2,
//   "expected_results": 4
// }
// ```
// - `status`: `"pending"` or `"completed"`.
// - `timed_out`: `true` when the response deadline passed with fewer results
//   than expected.
// - `received_results`: number of results submitted for this query.
// - `expected_results`: number of peers expected to respond.
//
// Errors:
// - `400`: missing/invalid JSON or malformed query ID.
// - `404`: query ID is not registered.
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
// `POST /search/get`
//
// On the initiating node the response also includes a locally computed result,
// so the initiator contributes its own matches. Any other node serves only the
// on-chain peer results and does not run a query it did not start.
//
// Request:
// ```json
// { "query_id": "8c62da8117dd88322d28cb4fb59b561007e3109476938e071732fee5f25bf399" }
// ```
//
// Response - `200 OK`:
// ```json
// {
//   "status": "completed",
//   "query_id": "8c62da8117dd88322d28cb4fb59b561007e3109476938e071732fee5f25bf399",
//   "completed": true,
//   "timed_out": false,
//   "verified_results": 3,
//   "rejected_results": 0,
//   "results": [],
//   "aggregated": [
//     {
//       "skill_digest": "0x51bd...",
//       "bm25_score": 1231569101,
//       "origin": "Node-ve3tav5jw5ico7or34gcb6ni7rtmd35uokriuijygabd3tsltd7fj6ia"
//     }
//   ]
// }
// ```
// Each entry in `results` is a `SearchResult` containing the submitting peer's
// `NodeID`, matches, BM25 scores, Merkle root, timestamp, and signature.
// `aggregated` deduplicates matches by skill digest, keeping the best BM25 score
// per skill and ordering by descending score then digest. Its `origin` field
// names the peer that supplied the retained score. Registry results are ordered
// by sender before aggregation and the initiator's own result is appended last,
// so an equal score keeps the smallest-sender peer, or the initiator if no peer
// matched. That origin lets a client confirm matches came from several nodes.
// It is computed locally from the result's signed sender and is not part of the
// on-chain `SearchResult`.
//
// Pending response - `409 Conflict`:
// ```json
// {
//   "status": "pending",
//   "query_id": "8c62da8117dd88322d28cb4fb59b561007e3109476938e071732fee5f25bf399",
//   "completed": false,
//   "received_results": 1,
//   "expected_results": 3,
//   "results": []
// }
// ```
//
// Errors:
// - `400`: missing/invalid JSON or malformed query ID.
// - `404`: query ID is not registered.
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

// Returns the full body of a skill by digest.
//
// `POST /skills/get`
//
// Request:
// ```json
// { "digest": "51bd2856abb0aab88c12350031ad60057ed89e2b528ca9ffb813e48ee6f27279" }
// ```
// The digest is the `skill_digest` / `digest` value returned by search and
// `/skills/list`; a leading `0x` is accepted.
//
// Response - `200 OK`: the same JSON skill structure accepted by
// `POST /skills/upload`.
// - `400 Bad Request`: missing, empty, or invalid digest.
// - `404 Not Found`: no skill with that digest is cached or loaded locally.
//
// Peer skills are served from the bodies persisted when they arrive during a
// search UPC; locally uploaded skills fall back to this node's catalog.
#[method::instance(export = http, method = "POST", path_prefix = "/skills/get")]
fn get_skill_at_skills_get(ctx: &mut _, req: http::Request) -> LyquidResult<http::Response> {
    handle_skill_get(&mut ctx, &req)
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
