//! A complete skill and its canonical representation.

use std::path::{Path, PathBuf};

use lyquid::prelude::Hash;
use serde::{Deserialize, Serialize};

use super::{MarkdownFile, SkillError, SkillResource, SkillResult};

/// Canonical integer representation of a BM25 score.
pub type Bm25Score = i64;

/// A skill together with its searchable metadata and resource files.
///
/// The layout follows `skill_core::Skill`, with the local filesystem `path`
/// deliberately omitted because it must not affect the canonical digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Stable skill identifier.
    pub id: String,
    /// Skill name.
    pub name: String,
    /// Short description of the skill.
    pub description: String,
    /// Phrases that should trigger the skill.
    pub triggers: Vec<String>,
    /// Markdown instructions for using the skill.
    pub instructions: String,
    /// Capabilities provided by the skill.
    pub capabilities: Vec<String>,
    /// Resource files shipped with the skill.
    pub resources: Vec<SkillResource>,
}

impl Skill {
    /// Creates a skill from its core metadata.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
        resources: Vec<SkillResource>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            triggers: Vec::new(),
            instructions: instructions.into(),
            capabilities: Vec::new(),
            resources,
        }
    }

    /// Loads a skill directory by discovering and parsing its `SKILL.md`.
    ///
    /// All other regular text files in the directory are loaded as resources.
    /// Hidden files and directories are ignored.
    pub fn from_dir(dir: impl AsRef<Path>) -> SkillResult<Self> {
        let dir = dir.as_ref();
        let manifest_path = find_skill_md(dir)?;
        let manifest = MarkdownFile::from_path(&manifest_path)?;
        let resources = load_resources(dir)?;
        Ok(Self::from_manifest(manifest, resources))
    }

    /// Creates a skill from a parsed Markdown manifest and resource files.
    pub fn from_manifest(manifest: MarkdownFile, resources: Vec<SkillResource>) -> Self {
        let frontmatter = manifest.frontmatter();
        let id = frontmatter
            .id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| frontmatter.name.clone());

        Self {
            id,
            name: frontmatter.name.clone(),
            description: frontmatter.description.clone(),
            triggers: frontmatter.triggers.clone(),
            instructions: manifest.body().to_owned(),
            capabilities: frontmatter.capabilities.clone(),
            resources,
        }
    }

    /// Sets the skill triggers.
    pub fn with_triggers(mut self, triggers: Vec<String>) -> Self {
        self.triggers = triggers;
        self
    }

    /// Sets the skill capabilities.
    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Returns the text used for local keyword search.
    ///
    /// Field order is fixed so callers can build a deterministic corpus.
    pub fn search_text(&self) -> String {
        let mut parts = vec![
            self.name.clone(),
            self.description.clone(),
            self.instructions.clone(),
        ];
        parts.extend(self.triggers.clone());
        parts.extend(self.capabilities.clone());
        parts.join(" ")
    }

    /// Encodes the skill into its canonical byte representation.
    pub fn canonical_bytes(&self) -> SkillResult<Vec<u8>> {
        unimplemented!("Skill::canonical_bytes")
    }

    /// Decodes a skill from its canonical byte representation.
    pub fn from_canonical_bytes(bytes: &[u8]) -> SkillResult<Self> {
        let _ = bytes;
        unimplemented!("Skill::from_canonical_bytes")
    }

    /// Computes the canonical digest of the skill.
    pub fn digest(&self) -> SkillResult<Hash> {
        unimplemented!("Skill::digest")
    }
}

fn find_skill_md(dir: &Path) -> SkillResult<PathBuf> {
    let exact = dir.join("SKILL.md");
    if exact.is_file() {
        return Ok(exact);
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
        {
            return Ok(entry.path());
        }
    }

    Err(SkillError::InvalidSkill(format!(
        "no SKILL.md found in {}",
        dir.display()
    )))
}

fn load_resources(root: &Path) -> SkillResult<Vec<SkillResource>> {
    let mut resources = Vec::new();
    collect_resources(root, root, &mut resources)?;
    resources.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(resources)
}

fn collect_resources(
    root: &Path,
    dir: &Path,
    resources: &mut Vec<SkillResource>,
) -> SkillResult<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_resources(root, &path, resources)?;
        } else if file_type.is_file() && !name.eq_ignore_ascii_case("SKILL.md") {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let relative = relative.to_string_lossy().replace('\\', "/");
            resources.push(SkillResource::from_path(&path, relative)?);
        }
    }

    Ok(())
}

/// A skill digest and its canonical BM25 score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredSkill {
    /// Digest of the matched skill.
    pub digest: Hash,
    /// Canonical BM25 score.
    pub score: Bm25Score,
}
