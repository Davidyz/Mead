//! Off-chain skill loading, canonicalization, and lookup.
//!
//! Skills are loaded from disk, split into their Markdown manifest and resource
//! files, and exposed through a read-only [`SkillManager`]. The manager is also
//! available as a process-wide singleton via [`SkillManager::init_shared`] and
//! [`SkillManager::shared`].

mod error;
mod file;
mod manager;
mod skill;

#[cfg(test)]
mod test_support;

pub use error::{SkillError, SkillResult};
pub use file::{Frontmatter, MarkdownFile, SkillResource, TextFile};
pub use manager::SkillManager;
pub use skill::{Bm25Score, ScoredSkill, Skill};
