//! Mead: blockchain-based P2P skill distribution platform.
//!
//! On-chain state consists of a skill directory and query/result registries
//! that track search queries and peer-submitted results.

use lyquid::prelude::lyquor_primitives::HashBytes;
use lyquid::prelude::*;
pub mod types;
pub mod verify;

use types::invalid_input;
use types::{QueryRegistry, ResultRegistry, SignedQuery, SignedResult};

state! {
    // On-chain registries
    instance skill_dir: Option<String> = None;
    network query_registry: QueryRegistry = QueryRegistry::default();
    network result_registry: ResultRegistry = ResultRegistry::default();
}

/// Sets this node's local skill directory.
///
/// Previously named `constructor`, but `constructor` must be a network function
/// per the LDK rules, while `skill_dir` is instance state. The old name collided
/// with Solidity's reserved `constructor` keyword in the generated stub and
/// blocked `shaker deploy` (solc ParserError 3323). Renamed to an instance setter
/// so it no longer collides; the value must now be passed by an explicit call
/// after deployment.
#[method::instance(export = eth)]
fn set_skill_dir(ctx: &mut _, skill_dir: String) -> LyquidResult<()> {
    {
        let mut dir = ctx.instance.skill_dir.write();
        *dir = Some(skill_dir);
    }
    Ok(())
}

// Registers a query and creates its responder set.
//
// The signature must be valid and the caller must be authorized to submit on
// behalf of the declared sender, otherwise any node could post a query under a
// forged address and have it accepted.
#[method::network]
fn register_query(ctx: &mut _, query: SignedQuery) -> LyquidResult<()> {
    verify::ensure_authorized(ctx.caller, query.sender)?;

    if !verify::verify_query(&query) {
        return Err(invalid_input("query signature is invalid"));
    }

    ctx.network.query_registry.insert(query)
}

// Validates and stores a peer result, then records the peer as a responder.
#[method::network]
fn submit_result(ctx: &mut _, result: SignedResult) -> LyquidResult<()> {
    verify::ensure_authorized(ctx.caller, result.sender)?;

    if !verify::verify_result(&result) {
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
fn get_query(ctx: &_, query_id: HashBytes) -> LyquidResult<Option<SignedQuery>> {
    let query_id = Hash::from(query_id);
    Ok(ctx.network.query_registry.get(&query_id).cloned())
}

// Returns a peer's result for a query.
#[method::network]
fn get_result(ctx: &_, query_id: HashBytes, sender: Address) -> LyquidResult<Option<SignedResult>> {
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

#[cfg(test)]
mod tests {
    #[lyquid_test::test]
    fn allocates_a_guest_string() {
        let greeting = String::from("hello");
        assert_eq!(greeting.as_str(), "hello");
    }
}
