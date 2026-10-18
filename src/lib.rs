//! Mead: blockchain-based P2P skill distribution platform.
//!
//! On-chain state consists of a skill directory and query/result registries
//! that track search queries and peer-submitted results.

use lyquid::prelude::*;
pub mod types;

use types::{QueryRegistry, ResultRegistry};

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

#[cfg(test)]
mod tests {
    #[lyquid_test::test]
    fn allocates_a_guest_string() {
        let greeting = String::from("hello");
        assert_eq!(greeting.as_str(), "hello");
    }
}
