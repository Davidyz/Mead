//! Process-wide access to the locally available skills.

use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use lyquid::prelude::{Hash, HashMap, new_hashmap};

use super::{ScoredSkill, Skill, SkillError, SkillResult};

static SHARED: OnceLock<Arc<SkillManager>> = OnceLock::new();

/// Catalog of the skills available on this node.
#[derive(Debug, Clone)]
pub struct SkillManager {
    skills: Arc<RwLock<HashMap<Hash, Arc<Skill>>>>,
}

impl SkillManager {
    /// Builds a catalog from already-loaded skills.
    ///
    /// This is independent of where the skills came from. Skills with the same
    /// digest are deduplicated.
    pub fn from_skills(skills: impl IntoIterator<Item = Skill>) -> SkillResult<Self> {
        let mut catalog = new_hashmap();
        for skill in skills {
            insert_skill(&mut catalog, skill)?;
        }
        Ok(Self {
            skills: Arc::new(RwLock::new(catalog)),
        })
    }

    /// Builds a catalog from already-loaded skills and installs it as the
    /// process-wide singleton.
    pub fn init_shared(skills: impl IntoIterator<Item = Skill>) -> SkillResult<Arc<Self>> {
        let manager = Arc::new(Self::from_skills(skills)?);
        SHARED
            .set(Arc::clone(&manager))
            .map_err(|_| SkillError::AlreadyInitialized)?;
        Ok(manager)
    }

    /// Returns the process-wide manager, if it has been initialized.
    pub fn shared() -> SkillResult<&'static Arc<Self>> {
        SHARED.get().ok_or(SkillError::NotInitialized)
    }

    /// Appends a skill to the catalog.
    ///
    /// The skill is validated and its digest is computed before insertion.
    /// Skills with an existing digest are ignored, matching catalog construction.
    pub fn append(&self, skill: Skill) -> SkillResult<()> {
        let mut skills = self.write_skills();
        insert_skill(&mut skills, skill)
    }

    /// Returns the number of loaded skills.
    pub fn len(&self) -> usize {
        self.read_skills().len()
    }

    /// Returns `true` if no skills are loaded.
    pub fn is_empty(&self) -> bool {
        self.read_skills().is_empty()
    }

    /// Returns a skill by digest.
    pub fn get(&self, digest: &Hash) -> Option<Arc<Skill>> {
        self.read_skills().get(digest).cloned()
    }

    /// Returns `true` if a skill with the given digest is loaded.
    pub fn contains(&self, digest: &Hash) -> bool {
        self.read_skills().contains_key(digest)
    }

    /// Returns a deterministic snapshot of all loaded skills.
    ///
    /// Items are ordered by digest, and the iterator owns its snapshot so no
    /// catalog lock is held while callers process the results.
    pub fn iter(&self) -> impl Iterator<Item = (Hash, Arc<Skill>)> {
        let mut snapshot: Vec<_> = self
            .read_skills()
            .iter()
            .map(|(digest, skill)| (*digest, Arc::clone(skill)))
            .collect();
        snapshot.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        snapshot.into_iter()
    }

    /// Returns the canonical encoded bundle for a skill.
    pub fn bundle(&self, digest: &Hash) -> SkillResult<Arc<[u8]>> {
        let skill = self.get(digest).ok_or(SkillError::NotFound)?;
        Ok(skill.canonical_bytes()?.into())
    }

    /// Runs a local search and returns the top-k skill digests.
    pub fn local_search(&self, query: &str, k: usize) -> SkillResult<Vec<Hash>> {
        let _ = (query, k);
        unimplemented!("SkillManager::local_search")
    }

    /// Computes canonical scores using exactly the supplied skills as the corpus.
    pub fn score_local_corpus(
        &self,
        query: &str,
        digests: &[Hash],
    ) -> SkillResult<Vec<ScoredSkill>> {
        let _ = (query, digests);
        unimplemented!("SkillManager::score_local_corpus")
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

fn insert_skill(catalog: &mut HashMap<Hash, Arc<Skill>>, skill: Skill) -> SkillResult<()> {
    let digest = skill.digest()?;
    catalog.entry(digest).or_insert_with(|| Arc::new(skill));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::test_support::{load_doc_coauthoring, load_internal_comms};
    use crate::skill::{Skill, SkillResource};

    fn manager_with(skills: Vec<Skill>) -> SkillManager {
        SkillManager::from_skills(skills).unwrap()
    }

    #[lyquid_test::test]
    fn from_skills_builds_an_in_memory_catalog() {
        let doc = load_doc_coauthoring();
        let comms = load_internal_comms();
        let doc_digest = doc.digest().unwrap();
        let comms_digest = comms.digest().unwrap();

        let manager = SkillManager::from_skills(vec![doc.clone(), comms, doc.clone()]).unwrap();

        assert_eq!(manager.len(), 2);
        assert!(manager.contains(&doc_digest));
        assert!(manager.contains(&comms_digest));
        assert_eq!(manager.get(&doc_digest).as_deref(), Some(&doc));
    }

    #[lyquid_test::test]
    fn from_skills_rejects_invalid_skills() {
        let resource = SkillResource::new("same.txt", "contents").unwrap();
        let invalid = Skill::new(
            "invalid",
            "Invalid",
            "Invalid skill",
            "Instructions",
            vec![resource.clone(), resource],
        );

        assert!(matches!(
            SkillManager::from_skills([invalid]),
            Err(SkillError::InvalidSkill(_))
        ));
    }

    #[lyquid_test::test]
    fn append_adds_skills_to_the_in_memory_catalog() {
        let doc = load_doc_coauthoring();
        let comms = load_internal_comms();
        let comms_digest = comms.digest().unwrap();
        let manager = manager_with(vec![doc.clone()]);

        manager.append(comms.clone()).unwrap();
        manager.append(doc).unwrap();

        assert_eq!(manager.len(), 2);
        assert_eq!(manager.get(&comms_digest).as_deref(), Some(&comms));
    }

    #[lyquid_test::test]
    fn append_rejects_invalid_skills_without_changing_the_catalog() {
        let resource = SkillResource::new("same.txt", "contents").unwrap();
        let invalid = Skill::new(
            "invalid",
            "Invalid",
            "Invalid skill",
            "Instructions",
            vec![resource.clone(), resource],
        );
        let manager = manager_with(vec![load_doc_coauthoring()]);

        assert!(matches!(
            manager.append(invalid),
            Err(SkillError::InvalidSkill(_))
        ));
        assert_eq!(manager.len(), 1);
    }

    #[lyquid_test::test]
    fn iter_returns_a_snapshot_sorted_by_digest() {
        let doc = load_doc_coauthoring();
        let comms = load_internal_comms();
        let manager = manager_with(vec![comms, doc]);

        let items: Vec<_> = manager.iter().collect();
        assert_eq!(items.len(), 2);
        assert!(items[0].0.as_bytes() <= items[1].0.as_bytes());
    }

    #[lyquid_test::test]
    fn get_contains_and_bundle_use_real_skills() {
        let doc = load_doc_coauthoring();
        let digest = doc.digest().unwrap();
        let expected = doc.canonical_bytes().unwrap();
        let manager = manager_with(vec![doc.clone()]);

        assert_eq!(manager.len(), 1);
        assert!(!manager.is_empty());
        assert!(manager.contains(&digest));
        assert_eq!(manager.get(&digest).as_deref(), Some(&doc));
        assert_eq!(&*manager.bundle(&digest).unwrap(), expected.as_slice());
    }

    #[lyquid_test::test]
    fn bundle_returns_not_found_for_unknown_skill() {
        let missing = load_internal_comms().digest().unwrap();
        let manager = manager_with(vec![load_doc_coauthoring()]);

        assert!(matches!(
            manager.bundle(&missing),
            Err(SkillError::NotFound)
        ));
    }

    #[lyquid_test::test]
    fn shared_manager_uses_real_catalog() {
        assert!(matches!(
            SkillManager::shared(),
            Err(SkillError::NotInitialized)
        ));

        let manager = SkillManager::init_shared(vec![load_doc_coauthoring()]).unwrap();

        assert!(Arc::ptr_eq(&manager, SkillManager::shared().unwrap()));
        assert!(matches!(
            SkillManager::init_shared([]),
            Err(SkillError::AlreadyInitialized)
        ));
    }
}
