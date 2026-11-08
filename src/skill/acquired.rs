//! Process-wide cache of skill bodies acquired from peers during search.
//!
//! This cache is intentionally separate from [`super::SkillManager`]: a skill
//! learned from a peer must be downloadable later without becoming part of this
//! node's local search corpus (which would change future BM25 results). It is
//! not persisted in instance state; it is rebuilt as peers answer new searches.

use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use lyquid::prelude::{Hash, HashMap, new_hashmap};

use super::Skill;

static ACQUIRED: OnceLock<AcquiredSkills> = OnceLock::new();

/// Digest-indexed cache of verified skill bodies learned from peer replies.
#[derive(Debug)]
pub(crate) struct AcquiredSkills {
    skills: RwLock<HashMap<Hash, Arc<Skill>>>,
}

impl AcquiredSkills {
    /// Creates an empty cache.
    pub(crate) fn new() -> Self {
        Self {
            skills: RwLock::new(new_hashmap()),
        }
    }

    /// Returns the process-wide cache used by the search and skills endpoints.
    pub(crate) fn shared() -> &'static Self {
        ACQUIRED.get_or_init(Self::new)
    }

    /// Records a verified skill body under its digest.
    ///
    /// Duplicate digests keep the first body. The digest must already match the
    /// body; callers verify that before recording.
    pub(crate) fn record(&self, digest: Hash, skill: Skill) {
        let mut skills = self.write_skills();
        skills.entry(digest).or_insert_with(|| Arc::new(skill));
    }

    /// Returns a cached skill by digest.
    pub(crate) fn get(&self, digest: &Hash) -> Option<Arc<Skill>> {
        self.read_skills().get(digest).cloned()
    }

    /// Returns the number of cached skills.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.read_skills().len()
    }

    /// Returns `true` when no skills have been acquired yet.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.read_skills().is_empty()
    }

    fn read_skills(&self) -> RwLockReadGuard<'_, HashMap<Hash, Arc<Skill>>> {
        self.skills
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_skills(&self) -> RwLockWriteGuard<'_, HashMap<Hash, Arc<Skill>>> {
        self.skills
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::test_support::load_doc_coauthoring;

    #[lyquid_test::test]
    fn records_and_fetches_skills_by_digest() {
        let cache = AcquiredSkills::new();
        let skill = load_doc_coauthoring();
        let digest = skill.digest().unwrap();

        cache.record(digest, skill.clone());

        assert_eq!(cache.len(), 1);
        assert!(!cache.is_empty());
        assert_eq!(cache.get(&digest).as_deref(), Some(&skill));
        assert!(cache.get(&Hash::from_bytes([0; 32])).is_none());
    }

    #[lyquid_test::test]
    fn duplicate_digests_keep_the_first_body() {
        let cache = AcquiredSkills::new();
        let skill = load_doc_coauthoring();
        let digest = skill.digest().unwrap();

        cache.record(digest, skill.clone());
        cache.record(digest, skill.clone());

        assert_eq!(cache.len(), 1);
    }
}
