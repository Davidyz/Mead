//! Off-chain skill parsing, canonicalization, and lookup.
//!
//! Skills are parsed from Markdown manifests and text resource files, then
//! exposed through a [`SkillManager`]. The manager is also available as a
//! process-wide singleton via [`SkillManager::init_shared`] and
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
