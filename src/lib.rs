//! Mead: blockchain-based P2P skill distribution platform.
//!
//! On-chain state consists of a skill directory and query/result registries
//! that track search queries and peer-submitted results.

use lyquid::prelude::lyquor_primitives::HashBytes;
use lyquid::prelude::*;
use std::sync::Arc;

pub mod skill;
pub mod types;
pub mod verify;

use skill::{Skill, SkillError, SkillManager};
use types::invalid_input;
use types::{QueryRegistry, ResultRegistry, SearchQuery, SearchResult};

state! {
    // On-chain registries
    network query_registry: QueryRegistry = QueryRegistry::default();
    network result_registry: ResultRegistry = ResultRegistry::default();
}

// Registers a query and creates its responder set.
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

    ctx.network.query_registry.insert(query)
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
    let body = serde_json::json!({ "count": count }).to_string();
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
    #[lyquid_test::test]
    fn allocates_a_guest_string() {
        let greeting = String::from("hello");
        assert_eq!(greeting.as_str(), "hello");
    }
}
