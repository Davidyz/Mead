//! A Merkle tree implementation using Blake3 hashing.
//!
//! Used to commit to a list of `(skill_digest, bm25_score)` tuples in search results.
//! The tree is built bottom-up from leaf hashes, with odd-length lists handled by
//! duplicating the last element.
//!
//! Uses the re-exported `blake3` from `lyquid::prelude` to ensure type compatibility
//! with on-chain digest types.

use lyquid::prelude::blake3;

pub use lyquid::prelude::Hash;

/// Serialize a `Hash` as a hex string with "0x" prefix.
pub fn serialize_hash<S>(hash: &Hash, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let hex = format!("0x{}", hex::encode(hash.as_bytes()));
    serializer.serialize_str(&hex)
}

/// Deserialize a `Hash` from a hex string with optional "0x" prefix.
pub fn deserialize_hash<'de, D>(deserializer: D) -> Result<Hash, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s = String::deserialize(deserializer)?;
    let hex_str = s.strip_prefix("0x").unwrap_or(&s);
    let bytes = hex::decode(hex_str).map_err(serde::de::Error::custom)?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| serde::de::Error::custom("Hash must be 32 bytes"))?;
    Ok(Hash::from_bytes(arr))
}

mod hex {
    /// Encode bytes as a lowercase hex string.
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// Decode a lowercase hex string to bytes.
    pub fn decode(hex: &str) -> Result<Vec<u8>, String> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
            .collect()
    }
}

/// A node in the Merkle tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MerkleNode {
    Leaf(Hash),
    Internal {
        left: Box<MerkleNode>,
        right: Box<MerkleNode>,
    },
}

/// A Merkle tree built from a list of 32-byte hash digests.
///
/// Supports building the tree, generating inclusion proofs, and verifying them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleTree {
    root: Hash,
    /// The leaf hashes in left-to-right order.
    leaves: Vec<Hash>,
    /// The tree structure for proof generation.
    tree: MerkleNode,
}

/// A sibling direction for Merkle proof verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiblingDirection {
    /// The sibling is on the left.
    Left,
    /// The sibling is on the right.
    Right,
}

/// An inclusion proof for a single leaf in the Merkle tree.
///
/// Contains the leaf hash, its index, and the sequence of sibling hashes
/// needed to recompute the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleProof {
    /// The hash of the leaf being proven.
    pub leaf_hash: Hash,
    /// The 0-based index of the leaf in the leaves list.
    pub leaf_index: usize,
    /// The sibling hashes and their directions, from leaf level up to the root.
    pub siblings: Vec<(Hash, SiblingDirection)>,
}

impl MerkleTree {
    /// Creates a new Merkle tree from a list of leaf hashes.
    ///
    /// # Panics
    ///
    /// Panics if `leaves` is empty.
    pub fn new(leaves: Vec<Hash>) -> Self {
        assert!(!leaves.is_empty(), "Merkle tree requires at least one leaf");

        let tree = Self::build_tree(&leaves);
        let root = Self::hash_node(&tree);

        Self { root, leaves, tree }
    }

    /// Returns the Merkle root hash.
    pub fn root(&self) -> Hash {
        self.root
    }

    /// Returns the number of leaves in the tree.
    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    /// Returns a copy of all leaf hashes.
    pub fn leaves(&self) -> Vec<Hash> {
        self.leaves.clone()
    }

    /// Generates an inclusion proof for the leaf at the given index.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds.
    pub fn prove(&self, index: usize) -> MerkleProof {
        assert!(
            index < self.leaves.len(),
            "Leaf index {} out of bounds (tree has {} leaves)",
            index,
            self.leaves.len()
        );

        let mut siblings = Vec::new();
        Self::collect_proof(&self.tree, index, &mut siblings);

        MerkleProof {
            leaf_hash: self.leaves[index],
            leaf_index: index,
            siblings,
        }
    }

    /// Verifies a Merkle proof against this tree's root.
    pub fn verify(&self, proof: &MerkleProof) -> bool {
        let computed_root = proof.compute_root();
        computed_root == self.root
    }

    /// Builds the Merkle tree structure from a list of leaf hashes.
    fn build_tree(leaves: &[Hash]) -> MerkleNode {
        if leaves.len() == 1 {
            return MerkleNode::Leaf(leaves[0]);
        }

        let mid = leaves.len() / 2;
        let left = Self::build_tree(&leaves[..mid]);
        let right = Self::build_tree(&leaves[mid..]);

        MerkleNode::Internal {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Computes the Blake3 hash of a MerkleNode.
    fn hash_node(node: &MerkleNode) -> Hash {
        match node {
            MerkleNode::Leaf(h) => *h,
            MerkleNode::Internal { left, right } => {
                let mut data = [0u8; 64];
                data[..32].copy_from_slice(&left.hash_bytes());
                data[32..].copy_from_slice(&right.hash_bytes());
                blake3::hash(&data)
            }
        }
    }

    /// Collects sibling hashes along the path from the root to the leaf at `index`.
    fn collect_proof(node: &MerkleNode, index: usize, siblings: &mut Vec<(Hash, SiblingDirection)>) {
        if let MerkleNode::Internal { left, right } = node {
            if index < left.leaf_count() {
                siblings.push((Hash::from_bytes(right.hash_bytes()), SiblingDirection::Right));
                Self::collect_proof(left, index, siblings);
            } else {
                siblings.push((Hash::from_bytes(left.hash_bytes()), SiblingDirection::Left));
                Self::collect_proof(right, index - left.leaf_count(), siblings);
            }
        }
    }
}

impl MerkleNode {
    /// Returns the hash bytes of this node.
    fn hash_bytes(&self) -> [u8; 32] {
        match self {
            MerkleNode::Leaf(h) => *h.as_bytes(),
            MerkleNode::Internal { .. } => *MerkleTree::hash_node(self).as_bytes(),
        }
    }

    /// Returns the number of leaves under this node.
    fn leaf_count(&self) -> usize {
        match self {
            MerkleNode::Leaf(_) => 1,
            MerkleNode::Internal { left, right } => left.leaf_count() + right.leaf_count(),
        }
    }
}

impl MerkleProof {
    /// Recomputes the Merkle root from this proof.
    ///
    /// Starting from the leaf hash, applies each sibling hash in sequence
    /// according to its direction to reconstruct the root.
    pub fn compute_root(&self) -> Hash {
        let mut current = self.leaf_hash;

        for (sibling_hash, direction) in self.siblings.iter().rev() {
            let mut data = [0u8; 64];
            match direction {
                SiblingDirection::Left => {
                    data[..32].copy_from_slice(sibling_hash.as_bytes());
                    data[32..].copy_from_slice(current.as_bytes());
                }
                SiblingDirection::Right => {
                    data[..32].copy_from_slice(current.as_bytes());
                    data[32..].copy_from_slice(sibling_hash.as_bytes());
                }
            }
            current = blake3::hash(&data);
        }

        current
    }

    /// Verifies this proof against a known root hash.
    pub fn verify_against_root(&self, expected_root: Hash) -> bool {
        self.compute_root() == expected_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_bytes(data: &[u8]) -> Hash {
        blake3::hash(data)
    }

    #[lyquid_test::test]
    #[ignore]
    fn single_leaf_tree() {
        let leaf = hash_bytes(b"leaf1");
        let tree = MerkleTree::new(vec![leaf]);
        assert_eq!(tree.root(), leaf);
        assert_eq!(tree.leaf_count(), 1);
    }

    #[lyquid_test::test]
    #[ignore]
    fn two_leaf_tree() {
        let left = hash_bytes(b"leaf1");
        let right = hash_bytes(b"leaf2");
        let tree = MerkleTree::new(vec![left, right]);

        // Root should be blake3(left || right)
        let expected = hash_bytes(&[&left.as_bytes()[..], &right.as_bytes()[..]].concat());
        assert_eq!(tree.root(), expected);
    }

    #[lyquid_test::test]
    #[ignore]
    fn four_leaf_tree() {
        let leaves: Vec<Hash> = (0..4)
            .map(|i| hash_bytes(&[i as u8]))
            .collect();
        let tree = MerkleTree::new(leaves.clone());

        // Verify each leaf's proof
        for i in 0..4 {
            let proof = tree.prove(i);
            assert!(tree.verify(&proof));
            assert_eq!(proof.leaf_hash, leaves[i]);
            assert_eq!(proof.leaf_index, i);
        }
    }

    #[lyquid_test::test]
    #[ignore]
    fn odd_leaf_count() {
        let leaves: Vec<Hash> = (0..3)
            .map(|i| hash_bytes(&[i as u8]))
            .collect();
        let tree = MerkleTree::new(leaves.clone());

        // Should not panic with 3 leaves
        for i in 0..3 {
            let proof = tree.prove(i);
            assert!(tree.verify(&proof));
        }
    }

    #[lyquid_test::test]
    #[ignore]
    fn invalid_proof_fails() {
        let leaves: Vec<Hash> = (0..4)
            .map(|i| hash_bytes(&[i as u8]))
            .collect();
        let tree = MerkleTree::new(leaves);

        let mut proof = tree.prove(0);
        // Tamper with the leaf hash
        let mut bytes = *proof.leaf_hash.as_bytes();
        bytes[0] ^= 0xFF;
        proof.leaf_hash = Hash::from_bytes(bytes);

        assert!(!tree.verify(&proof));
    }

    #[lyquid_test::test]
    #[ignore]
    fn proof_verification_independent_of_tree() {
        let leaves: Vec<Hash> = (0..8)
            .map(|i| hash_bytes(&[i as u8]))
            .collect();
        let tree = MerkleTree::new(leaves.clone());

        for i in 0..8 {
            let proof = tree.prove(i);
            // Verify using the root directly, without the tree
            assert!(proof.verify_against_root(tree.root()));
        }
    }

    #[lyquid_test::test]
    #[ignore]
    fn large_tree() {
        let leaves: Vec<Hash> = (0..100)
            .map(|i| hash_bytes(&(i as u32).to_le_bytes()))
            .collect();
        let tree = MerkleTree::new(leaves);

        // Verify a few random proofs
        for i in [0, 49, 50, 99] {
            let proof = tree.prove(i);
            assert!(tree.verify(&proof));
        }
    }
}
