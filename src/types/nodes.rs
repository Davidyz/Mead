//! On-chain registry of nodes that take part in searches.
//!
//! Nodes self-register through the `register_node` network function, which binds
//! the caller's on-chain address to its Ed25519 node identity. `register_query`
//! snapshots this registry to decide which peers must be notified about a new
//! search, so the registry is consensus state rather than node-local state.

use lyquid::prelude::*;

use crate::types::invalid_input;

/// Maximum number of nodes accepted by the registry.
///
/// Registration is permissionless, so the cap bounds both the consensus state
/// and the fan-out of search announcements.
pub const MAX_REGISTERED_NODES: usize = 64;

/// Registry of nodes that can be asked to process searches.
#[derive(Debug, Clone)]
pub struct NodeRegistry {
    nodes: HashSet<NodeID>,
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self {
            nodes: new_hashset(),
        }
    }
}

impl NodeRegistry {
    /// Registers a node, rejecting new nodes once the registry is full.
    ///
    /// Registering an already-registered node is a no-op.
    pub fn register(&mut self, node: NodeID) -> LyquidResult<()> {
        if self.nodes.contains(&node) {
            return Ok(());
        }
        if self.nodes.len() >= MAX_REGISTERED_NODES {
            return Err(invalid_input("node registry is full"));
        }

        self.nodes.insert(node);
        Ok(())
    }

    /// Removes a node, returning `true` if it was registered.
    pub fn unregister(&mut self, node: NodeID) -> bool {
        self.nodes.remove(&node)
    }

    /// Returns true if the node is registered.
    pub fn contains(&self, node: &NodeID) -> bool {
        self.nodes.contains(node)
    }

    /// Returns the registered nodes.
    pub fn nodes(&self) -> &HashSet<NodeID> {
        &self.nodes
    }

    /// Returns the number of registered nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns true if no nodes are registered.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(byte: u8) -> NodeID {
        NodeID::new([byte; 32])
    }

    #[lyquid_test::test]
    fn registering_a_node_is_idempotent() {
        let mut registry = NodeRegistry::default();
        registry.register(node(1)).unwrap();
        registry.register(node(1)).unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&node(1)));
    }

    #[lyquid_test::test]
    fn rejects_registration_when_full() {
        let mut registry = NodeRegistry::default();
        for byte in 0..MAX_REGISTERED_NODES as u8 {
            registry.register(node(byte)).unwrap();
        }
        assert_eq!(registry.len(), MAX_REGISTERED_NODES);
        assert!(registry.register(node(200)).is_err());
        assert_eq!(registry.len(), MAX_REGISTERED_NODES);
    }

    #[lyquid_test::test]
    fn unregistering_releases_a_slot() {
        let mut registry = NodeRegistry::default();
        registry.register(node(1)).unwrap();
        assert!(registry.unregister(node(1)));
        assert!(!registry.unregister(node(1)));
        assert!(!registry.contains(&node(1)));
    }

    #[lyquid_test::test]
    fn unregistered_nodes_are_not_contained() {
        let registry = NodeRegistry::default();
        assert!(registry.is_empty());
        assert!(!registry.contains(&node(2)));
    }
}
