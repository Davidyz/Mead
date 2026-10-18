//! Mead: blockchain-based P2P skill distribution platform.
//!
//! On-chain state consists of a skill directory and query/result registries
//! that track search queries and peer-submitted results.

use lyquid::prelude::lyquor_primitives::HashBytes;
use lyquid::prelude::*;
pub mod types;

use types::{QueryRegistry, ResultRegistry, SignedQuery, SignedResult};

state! {
    // On-chain registries
    instance skill_dir: Option<String> = None;
    network query_registry: QueryRegistry = QueryRegistry::default();
    network result_registry: ResultRegistry = ResultRegistry::default();
}

#[method::instance(export = eth)]
fn constructor(ctx: &mut _, skill_dir: String) -> LyquidResult<()> {
    {
        let mut dir = ctx.instance.skill_dir.write();
        *dir = Some(skill_dir);
    }
    Ok(())
}

// Registers a query and creates its responder set.
#[method::network]
fn register_query(ctx: &mut _, query: SignedQuery) -> LyquidResult<()> {
    ctx.network.query_registry.insert(query)
}

// Validates and stores a peer result, then records the peer as a responder.
#[method::network]
fn submit_result(ctx: &mut _, result: SignedResult) -> LyquidResult<()> {
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
