//! Process-wide access to the locally available skills.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard};

use lyquid::prelude::{Hash, HashMap, new_hashmap};

use super::{ScoredSkill, Skill, SkillError, SkillResult};

static SHARED: OnceLock<Arc<SkillManager>> = OnceLock::new();

/// Read-only catalog of the skills available on this node.
#[derive(Debug, Clone)]
pub struct SkillManager {
    root: PathBuf,
    skills: Arc<RwLock<HashMap<Hash, Arc<Skill>>>>,
}

impl SkillManager {
    /// Loads all skills under `root`.
    pub fn load(root: impl Into<PathBuf>) -> SkillResult<Self> {
        let root = root.into();
        let skills = load_catalog(&root)?;
        Ok(Self {
            root,
            skills: Arc::new(RwLock::new(skills)),
        })
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
        let skills = load_catalog(&self.root)?;
        *self
            .skills
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = skills;
        Ok(())
    }

    /// Returns the root directory scanned by this manager.
    pub fn root(&self) -> &Path {
        &self.root
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
    // TODO: expose an iterator/digest listing for user-facing skill enumeration.
    pub fn get(&self, digest: &Hash) -> Option<Arc<Skill>> {
        self.read_skills().get(digest).cloned()
    }

    /// Returns `true` if a skill with the given digest is loaded.
    pub fn contains(&self, digest: &Hash) -> bool {
        self.read_skills().contains_key(digest)
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
}

fn load_catalog(root: &Path) -> SkillResult<HashMap<Hash, Arc<Skill>>> {
    // TODO: skip/report malformed or hidden child directories instead of failing the whole catalog.
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.path());
        }
    }
    directories.sort();

    let mut skills = new_hashmap();
    for directory in directories {
        let skill = Skill::from_dir(&directory)?;
        let digest = skill.digest()?;
        // TODO: document/verify duplicate skill digests from different directories.
        skills.entry(digest).or_insert_with(|| Arc::new(skill));
    }
    Ok(skills)
}

// TODO: add fixture-based tests for load/refresh/bundle, duplicate digests, and singleton behavior.
