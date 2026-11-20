//! Node bonds used by the peer penalty mechanism.
//!
//! A node posts a token bond before it can participate in searches. The
//! initiating node can ask the search oracle to slash that bond when a peer
//! returns a result that is signed by the peer but fails result validation.
//! Bonds are accounting state; the token transfer itself happens in the
//! network functions in `lib.rs`.

use lyquid::prelude::*;

use crate::types::invalid_input;

/// Basis-point denominator for fractional slashing (100% = 10_000).
pub const BPS_DENOMINATOR: u16 = 10_000;

/// Bond accounting for one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondInfo {
    /// Currently bonded token amount.
    pub amount: U256,
    /// Unix timestamp (seconds) of the first deposit.
    pub posted_at: u64,
}

/// Registry of node bonds.
#[derive(Debug, Clone)]
pub struct BondRegistry {
    bonds: HashMap<NodeID, BondInfo>,
}

impl Default for BondRegistry {
    fn default() -> Self {
        Self {
            bonds: new_hashmap(),
        }
    }
}

impl BondRegistry {
    /// Adds `amount` to a node's bond.
    pub fn post(&mut self, node: NodeID, amount: U256, posted_at: u64) -> LyquidResult<()> {
        if amount.is_zero() {
            return Err(invalid_input("bond amount must be greater than zero"));
        }

        match self.bonds.get_mut(&node) {
            Some(info) => info.amount += amount,
            None => {
                self.bonds.insert(node, BondInfo { amount, posted_at });
            }
        }
        Ok(())
    }

    /// Slashes `fraction_bps` of a node's bond and returns the slashed amount.
    ///
    /// A node whose bond reaches zero is removed from the registry. The caller
    /// decides whether to remove the node from the search node registry.
    pub fn slash(&mut self, node: NodeID, fraction_bps: u16) -> LyquidResult<U256> {
        if fraction_bps == 0 || fraction_bps > BPS_DENOMINATOR {
            return Err(invalid_input(
                "slash fraction must be between 1 and 10_000 bps",
            ));
        }

        let info = self
            .bonds
            .get_mut(&node)
            .ok_or_else(|| invalid_input("node has no bond"))?;
        // Split the division so large bonds cannot overflow while multiplying
        // by the basis-point fraction.
        let denominator = U256::from(BPS_DENOMINATOR as u64);
        let fraction = U256::from(fraction_bps as u64);
        let slashed = (info.amount / denominator) * fraction
            + (info.amount % denominator) * fraction / denominator;
        info.amount -= slashed;

        if info.amount.is_zero() {
            self.bonds.remove(&node);
        }
        Ok(slashed)
    }

    /// Removes and returns a node's remaining bond.
    pub fn withdraw(&mut self, node: NodeID) -> U256 {
        self.bonds
            .remove(&node)
            .map(|info| info.amount)
            .unwrap_or(U256::ZERO)
    }

    /// Returns a node's current bond amount.
    pub fn amount(&self, node: NodeID) -> U256 {
        self.bonds
            .get(&node)
            .map(|info| info.amount)
            .unwrap_or(U256::ZERO)
    }

    /// Returns the node's bond entry, if present.
    pub fn get(&self, node: &NodeID) -> Option<&BondInfo> {
        self.bonds.get(node)
    }

    /// Returns true when the node's bond meets `minimum`.
    pub fn is_eligible(&self, node: NodeID, minimum: U256) -> bool {
        self.amount(node) >= minimum
    }

    /// Returns the number of bonded nodes.
    pub fn len(&self) -> usize {
        self.bonds.len()
    }

    /// Returns true when no bonds are recorded.
    pub fn is_empty(&self) -> bool {
        self.bonds.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(byte: u8) -> NodeID {
        NodeID::new([byte; 32])
    }

    #[lyquid_test::test]
    fn posting_adds_to_the_existing_bond() {
        let mut bonds = BondRegistry::default();
        bonds.post(node(1), U256::from(10u64), 1).unwrap();
        bonds.post(node(1), U256::from(5u64), 2).unwrap();

        let info = bonds.get(&node(1)).unwrap();
        assert_eq!(info.amount, U256::from(15u64));
        assert_eq!(info.posted_at, 1);
    }

    #[lyquid_test::test]
    fn slashing_reduces_the_bond_and_removes_depleted_nodes() {
        let mut bonds = BondRegistry::default();
        bonds.post(node(1), U256::from(10u64), 1).unwrap();

        assert_eq!(bonds.slash(node(1), 5_000).unwrap(), U256::from(5u64));
        assert_eq!(bonds.amount(node(1)), U256::from(5u64));

        assert_eq!(bonds.slash(node(1), 10_000).unwrap(), U256::from(5u64));
        assert!(bonds.get(&node(1)).is_none());
        assert!(!bonds.is_eligible(node(1), U256::from(1u64)));
    }

    #[lyquid_test::test]
    fn withdrawing_returns_and_clears_the_bond() {
        let mut bonds = BondRegistry::default();
        bonds.post(node(1), U256::from(7u64), 1).unwrap();

        assert_eq!(bonds.withdraw(node(1)), U256::from(7u64));
        assert_eq!(bonds.withdraw(node(1)), U256::ZERO);
        assert!(bonds.is_empty());
    }

    #[lyquid_test::test]
    fn rejects_invalid_bond_operations() {
        let mut bonds = BondRegistry::default();
        assert!(bonds.post(node(1), U256::ZERO, 1).is_err());
        assert!(bonds.slash(node(1), 1).is_err());

        bonds.post(node(1), U256::from(1u64), 1).unwrap();
        assert!(bonds.slash(node(1), 0).is_err());
        assert!(bonds.slash(node(1), BPS_DENOMINATOR + 1).is_err());
    }
}
