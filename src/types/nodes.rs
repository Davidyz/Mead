//! On-chain registry of nodes that take part in searches.
//!
//! Nodes self-register through the `register_node` network function, which binds
//! the caller's on-chain address to its Ed25519 node identity. `register_query`
//! snapshots this registry to decide which peers must be notified about a new
//! search, so the registry is consensus state rather than node-local state.

use lyquid::prelude::*;

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
    /// Registers a node, returning `true` if it was not registered before.
    pub fn register(&mut self, node: NodeID) -> bool {
        self.nodes.insert(node)
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
        assert!(registry.register(node(1)));
        assert!(!registry.register(node(1)));
        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&node(1)));
    }

    #[lyquid_test::test]
    fn unregistered_nodes_are_not_contained() {
        let registry = NodeRegistry::default();
        assert!(registry.is_empty());
        assert!(!registry.contains(&node(2)));
    }
}
