//! File-backed skill contents.
//!
//! [`TextFile`] owns the raw bytes of a file together with its content digest.
//! [`SkillResource`] attaches a relative path to a [`TextFile`].
//! [`MarkdownFile`] is a [`TextFile`] that has been split into YAML frontmatter
//! and Markdown body.

use std::collections::BTreeMap;
use std::path::Path;

use lyquid::prelude::Hash;
use lyquid::prelude::blake3;
use lyquid::prelude::lyquor_primitives::HashBytes;
use serde::{Deserialize, Serialize};

use super::{SkillError, SkillResult};

/// UTF-8 text content and its content digest.
// TODO: support binary/non-UTF-8 assets. `TextFile` requires UTF-8, so skills
// containing binary resources fail to load individually; `SkillManager` skips
// such skills. Decide the canonical representation before changing the v1
// resource schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextFile {
    /// File contents.
    contents: String,
    /// Digest of `contents`.
    digest: HashBytes,
}

impl TextFile {
    /// Loads text content from disk.
    ///
    /// Files that are not valid UTF-8 are rejected with [`SkillError::NonTextFile`].
    pub fn from_path(path: impl AsRef<Path>) -> SkillResult<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        let contents =
            String::from_utf8(bytes).map_err(|_| SkillError::NonTextFile(path.to_path_buf()))?;
        Self::new(contents)
    }

    /// Creates a text file from its contents.
    // TODO: this currently cannot fail; simplify the signature or reserve it for future validation.
    pub fn new(contents: impl Into<String>) -> SkillResult<Self> {
        let contents = contents.into();
        let digest = HashBytes::from(blake3::hash(contents.as_bytes()));
        Ok(Self { contents, digest })
    }

    /// Returns the file contents as a string.
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// Returns the file contents as bytes.
    pub fn bytes(&self) -> &[u8] {
        self.contents.as_bytes()
    }

    /// Returns the content digest.
    pub fn digest(&self) -> Hash {
        Hash::from(self.digest)
    }

    /// Computes the digest of arbitrary file contents.
    pub fn compute_digest(contents: &[u8]) -> Hash {
        blake3::hash(contents)
    }

    /// Returns whether the stored digest matches the current contents.
    pub fn verify(&self) -> bool {
        self.digest == HashBytes::from(blake3::hash(self.contents.as_bytes()))
    }
}

/// A skill resource: a relative path and its file contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillResource {
    /// Path relative to the skill directory, using `/` separators.
    pub path: String,
    /// Resource file contents.
    pub file: TextFile,
}

impl SkillResource {
    /// Creates a resource from its relative path and contents.
    pub fn new(path: impl Into<String>, contents: impl Into<String>) -> SkillResult<Self> {
        Ok(Self {
            path: path.into(),
            file: TextFile::new(contents)?,
        })
    }

    /// Loads a resource from disk while storing a relative path.
    pub fn from_path(
        path: impl AsRef<Path>,
        relative_path: impl Into<String>,
    ) -> SkillResult<Self> {
        Ok(Self {
            path: relative_path.into(),
            file: TextFile::from_path(path)?,
        })
    }

    /// Validates the relative path and content digest.
    pub fn validate(&self) -> SkillResult<()> {
        if !is_valid_relative_path(&self.path) {
            return Err(SkillError::InvalidSkill(format!(
                "invalid resource path: {}",
                self.path
            )));
        }
        if !self.file.verify() {
            return Err(SkillError::InvalidSkill(format!(
                "resource digest does not match contents: {}",
                self.path
            )));
        }
        Ok(())
    }
}

fn is_valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Parsed YAML frontmatter from a Markdown skill file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Optional stable skill identifier.
    #[serde(default)]
    pub id: Option<String>,
    /// Skill name from the frontmatter.
    pub name: String,
    /// Short description of the skill.
    pub description: String,
    /// Phrases that should trigger the skill.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Capabilities provided by the skill.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Arbitrary string metadata.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// Additional frontmatter fields, preserved for forward compatibility.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// A Markdown file split into frontmatter and body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownFile {
    file: TextFile,
    frontmatter: Frontmatter,
    body: String,
}

impl MarkdownFile {
    /// Parses a Markdown file into YAML frontmatter and body.
    pub fn parse(file: TextFile) -> SkillResult<Self> {
        let (frontmatter, body) = markdown_frontmatter::parse::<Frontmatter>(file.contents())
            .map_err(|error| SkillError::InvalidFrontmatter(error.to_string()))?;
        let body = body.to_owned();

        Ok(Self {
            file,
            frontmatter,
            body,
        })
    }

    /// Loads and parses a Markdown file from disk.
    pub fn from_path(path: impl AsRef<Path>) -> SkillResult<Self> {
        Self::parse(TextFile::from_path(path)?)
    }

    /// Returns the underlying text file.
    pub fn text_file(&self) -> &TextFile {
        &self.file
    }

    /// Returns the parsed frontmatter.
    pub fn frontmatter(&self) -> &Frontmatter {
        &self.frontmatter
    }

    /// Returns the Markdown body after the frontmatter.
    pub fn body(&self) -> &str {
        &self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[lyquid_test::test]
    fn parses_skill_frontmatter() {
        let source = "---\nname: demo\ndescription: A demo skill\ntriggers:\n  - demo\ncapabilities:\n  - testing\n---\nUse this skill.\n";
        let file = TextFile::new(source).unwrap();
        let markdown = MarkdownFile::parse(file).unwrap();

        assert_eq!(markdown.frontmatter().name, "demo");
        assert_eq!(markdown.frontmatter().description, "A demo skill");
        assert_eq!(markdown.frontmatter().triggers, vec!["demo"]);
        assert_eq!(markdown.frontmatter().capabilities, vec!["testing"]);
        assert_eq!(markdown.body(), "Use this skill.\n");
    }

    #[lyquid_test::test]
    fn text_file_digest_matches_contents() {
        let file = TextFile::new("hello").unwrap();

        assert!(file.verify());
        assert_eq!(file.digest(), TextFile::compute_digest(b"hello"));
    }

    #[lyquid_test::test]
    fn resource_validation_rejects_unsafe_paths() {
        for path in ["/absolute", "../parent", "a/../../b", "a//b", "a\\b"] {
            let resource = SkillResource::new(path, "contents").unwrap();
            assert!(resource.validate().is_err(), "accepted unsafe path {path}");
        }
    }
}
