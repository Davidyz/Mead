//! Process-wide access to the locally available skills.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use lyquid::prelude::{Hash, HashMap};

use super::{ScoredSkill, Skill, SkillError, SkillResult};

static SHARED: OnceLock<Arc<SkillManager>> = OnceLock::new();

/// Read-only catalog of the skills available on this node.
#[derive(Debug, Clone)]
pub struct SkillManager {
    root: PathBuf,
    skills: HashMap<Hash, Arc<Skill>>,
}

impl SkillManager {
    /// Loads all skills under `root`.
    pub fn load(root: impl Into<PathBuf>) -> SkillResult<Self> {
        let _ = root.into();
        unimplemented!("SkillManager::load")
    }

    /// Loads all skills and installs the manager as the process-wide singleton.
    pub fn init_shared(root: impl Into<PathBuf>) -> SkillResult<Arc<Self>> {
        let manager = Arc::new(Self::load(root)?);
        SHARED
            .set(Arc::clone(&manager))
            .map_err(|_| SkillError::AlreadyInitialized)?;
        Ok(manager)
    }

    /// Returns the process-wide manager, if it has been initialized.
    pub fn shared() -> SkillResult<&'static Arc<Self>> {
        SHARED.get().ok_or(SkillError::NotInitialized)
    }

    /// Rescans the skill directory and replaces the in-memory catalog.
    pub fn refresh(&self) -> SkillResult<()> {
        unimplemented!("SkillManager::refresh")
    }

    /// Returns the root directory scanned by this manager.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the number of loaded skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Returns `true` if no skills are loaded.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Returns a skill by digest.
    pub fn get(&self, digest: &Hash) -> Option<Arc<Skill>> {
        self.skills.get(digest).cloned()
    }

    /// Returns `true` if a skill with the given digest is loaded.
    pub fn contains(&self, digest: &Hash) -> bool {
        self.skills.contains_key(digest)
    }

    /// Returns the canonical encoded bundle for a skill.
    pub fn bundle(&self, digest: &Hash) -> SkillResult<Arc<[u8]>> {
        let _ = digest;
        unimplemented!("SkillManager::bundle")
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
}
