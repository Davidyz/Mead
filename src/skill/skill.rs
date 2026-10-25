//! A complete skill and its canonical representation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lyquid::prelude::Hash;
use lyquid::prelude::blake3;
use serde::{Deserialize, Serialize};

use super::{MarkdownFile, SkillError, SkillResource, SkillResult};

/// Canonical, postcard-compatible serialization for arbitrary frontmatter values.
///
/// `serde_json::Value` cannot be deserialized by postcard because it uses
/// `deserialize_any`, so each value is stored as its canonical JSON text.
mod extra_serde {
    use std::collections::BTreeMap;

    use serde::de::Error as DeError;
    use serde::ser::Error as SerError;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(
        extra: &BTreeMap<String, serde_json::Value>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut encoded = BTreeMap::new();
        for (key, value) in extra {
            encoded.insert(key, canonical_json(value).map_err(SerError::custom)?);
        }
        encoded.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<String, serde_json::Value>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = BTreeMap::<String, String>::deserialize(deserializer)?;
        let mut extra = BTreeMap::new();
        for (key, encoded_value) in encoded {
            let value: serde_json::Value =
                serde_json::from_str(&encoded_value).map_err(DeError::custom)?;
            if canonical_json(&value).map_err(DeError::custom)? != encoded_value {
                return Err(DeError::custom("non-canonical extra JSON value"));
            }
            extra.insert(key, value);
        }
        Ok(extra)
    }

    fn canonical_json(value: &serde_json::Value) -> serde_json::Result<String> {
        use serde_json::Value;

        match value {
            Value::Null => Ok("null".to_owned()),
            Value::Bool(value) => Ok(value.to_string()),
            Value::Number(value) => Ok(value.to_string()),
            Value::String(value) => serde_json::to_string(value),
            Value::Array(values) => {
                let mut encoded = String::from("[");
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        encoded.push(',');
                    }
                    encoded.push_str(&canonical_json(value)?);
                }
                encoded.push(']');
                Ok(encoded)
            }
            Value::Object(values) => {
                let mut keys: Vec<_> = values.keys().collect();
                keys.sort();
                let mut encoded = String::from("{");
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        encoded.push(',');
                    }
                    encoded.push_str(&serde_json::to_string(key)?);
                    encoded.push(':');
                    let value = values
                        .get(key.as_str())
                        .expect("object key must exist in its own map");
                    encoded.push_str(&canonical_json(value)?);
                }
                encoded.push('}');
                Ok(encoded)
            }
        }
    }
}

/// Version of the canonical skill encoding.
const CANONICAL_VERSION: u8 = 1;
/// Domain separation for skill digests.
const DIGEST_DOMAIN: &[u8] = b"mead/skill/v1";

/// Canonical integer representation of a BM25 score.
// TODO: unify this alias with the on-chain `SkillMatch::bm25_score` type when the scoring PR lands.
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
    /// Arbitrary string metadata from the manifest frontmatter.
    pub metadata: BTreeMap<String, String>,
    /// Additional frontmatter fields preserved for forward compatibility.
    #[serde(with = "extra_serde")]
    pub extra: BTreeMap<String, serde_json::Value>,
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
            metadata: BTreeMap::new(),
            extra: BTreeMap::new(),
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
            metadata: frontmatter.metadata.clone(),
            extra: frontmatter.extra.clone(),
            resources,
        }
    }

    /// Sets the skill metadata.
    pub fn with_metadata(mut self, metadata: BTreeMap<String, String>) -> Self {
        self.metadata = metadata;
        self
    }

    /// Sets additional preserved frontmatter fields.
    pub fn with_extra(mut self, extra: BTreeMap<String, serde_json::Value>) -> Self {
        self.extra = extra;
        self
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
        let canonical = self.canonicalized()?;
        let encoded = postcard::to_allocvec(&canonical)
            .map_err(|error| SkillError::InvalidSkill(error.to_string()))?;

        let mut bytes = Vec::with_capacity(1 + encoded.len());
        bytes.push(CANONICAL_VERSION);
        bytes.extend_from_slice(&encoded);
        Ok(bytes)
    }

    /// Decodes a skill from its canonical byte representation.
    pub fn from_canonical_bytes(bytes: &[u8]) -> SkillResult<Self> {
        let (&version, encoded) = bytes
            .split_first()
            .ok_or_else(|| SkillError::InvalidSkill("empty canonical skill encoding".into()))?;
        if version != CANONICAL_VERSION {
            return Err(SkillError::InvalidSkill(format!(
                "unsupported canonical skill version: {version}"
            )));
        }

        let (skill, remaining) = postcard::take_from_bytes::<Self>(encoded)
            .map_err(|error| SkillError::InvalidSkill(error.to_string()))?;
        if !remaining.is_empty() {
            return Err(SkillError::InvalidSkill(
                "trailing bytes after canonical skill encoding".into(),
            ));
        }
        skill.validate_canonical()?;
        Ok(skill)
    }

    /// Computes the canonical digest of the skill.
    ///
    /// The returned [`Hash`] is intended for local/off-chain use. When passing
    /// it through a Lyquid contract method, convert it to
    /// `lyquid::prelude::lyquor_primitives::HashBytes` with `.into()`; `Hash`
    /// itself does not implement the serde traits required by Lyquid's postcard
    /// encoding.
    pub fn digest(&self) -> SkillResult<Hash> {
        let bytes = self.canonical_bytes()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(DIGEST_DOMAIN);
        hasher.update(&bytes);
        Ok(hasher.finalize())
    }

    fn canonicalized(&self) -> SkillResult<Self> {
        let mut canonical = self.clone();
        canonical
            .resources
            .sort_by(|left, right| left.path.cmp(&right.path));
        canonical.validate_canonical()?;
        Ok(canonical)
    }

    fn validate_canonical(&self) -> SkillResult<()> {
        for resource in &self.resources {
            resource.validate()?;
        }
        for pair in self.resources.windows(2) {
            if pair[0].path >= pair[1].path {
                return Err(SkillError::InvalidSkill(
                    "skill resources are not in canonical path order".into(),
                ));
            }
        }
        Ok(())
    }
}

fn find_skill_md(dir: &Path) -> SkillResult<PathBuf> {
    let exact = dir.join("SKILL.md");
    if exact.is_file() {
        return Ok(exact);
    }

    let mut candidates = Vec::new();
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
            candidates.push(entry.path());
        }
    }

    match candidates.len() {
        0 => Err(SkillError::InvalidSkill(format!(
            "no SKILL.md found in {}",
            dir.display()
        ))),
        1 => Ok(candidates.remove(0)),
        _ => Err(SkillError::InvalidSkill(format!(
            "multiple case-insensitive SKILL.md candidates in {}",
            dir.display()
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::TextFile;

    fn skill(resources: Vec<SkillResource>) -> Skill {
        Skill::new("demo", "Demo", "A demo skill", "Use it.", resources)
    }

    fn load_doc_coauthoring() -> Skill {
        let manifest = MarkdownFile::parse(
            TextFile::new(include_str!(
                "../../tests/skills/skills/doc-coauthoring/SKILL.md"
            ))
            .unwrap(),
        )
        .unwrap();
        Skill::from_manifest(manifest, Vec::new())
    }

    fn load_internal_comms() -> Skill {
        let mut resources = internal_comms_resources();
        resources.sort_by(|left, right| left.path.cmp(&right.path));

        Skill::from_manifest(internal_comms_manifest(), resources)
    }

    fn internal_comms_manifest() -> MarkdownFile {
        MarkdownFile::parse(
            TextFile::new(include_str!(
                "../../tests/skills/skills/internal-comms/SKILL.md"
            ))
            .unwrap(),
        )
        .unwrap()
    }

    fn internal_comms_resources() -> Vec<SkillResource> {
        vec![
            SkillResource::new(
                "examples/general-comms.md",
                include_str!("../../tests/skills/skills/internal-comms/examples/general-comms.md"),
            )
            .unwrap(),
            SkillResource::new(
                "LICENSE.txt",
                include_str!("../../tests/skills/skills/internal-comms/LICENSE.txt"),
            )
            .unwrap(),
            SkillResource::new(
                "examples/3p-updates.md",
                include_str!("../../tests/skills/skills/internal-comms/examples/3p-updates.md"),
            )
            .unwrap(),
            SkillResource::new(
                "examples/company-newsletter.md",
                include_str!(
                    "../../tests/skills/skills/internal-comms/examples/company-newsletter.md"
                ),
            )
            .unwrap(),
            SkillResource::new(
                "examples/faq-answers.md",
                include_str!("../../tests/skills/skills/internal-comms/examples/faq-answers.md"),
            )
            .unwrap(),
        ]
    }

    fn assert_real_skill_canonical_round_trip(fresh: Skill, reloaded: Skill) {
        let bytes = fresh.canonical_bytes().unwrap();
        let decoded = Skill::from_canonical_bytes(&bytes).unwrap();

        assert_eq!(decoded, reloaded);
        assert_eq!(decoded.id, reloaded.id);
        assert_eq!(decoded.name, reloaded.name);
        assert_eq!(decoded.description, reloaded.description);
        assert_eq!(decoded.triggers, reloaded.triggers);
        assert_eq!(decoded.instructions, reloaded.instructions);
        assert_eq!(decoded.capabilities, reloaded.capabilities);
        assert_eq!(decoded.metadata, reloaded.metadata);
        assert_eq!(decoded.extra, reloaded.extra);
        assert_eq!(decoded.resources.len(), reloaded.resources.len());
        for (decoded_resource, reloaded_resource) in
            decoded.resources.iter().zip(&reloaded.resources)
        {
            assert_eq!(decoded_resource.path, reloaded_resource.path);
            assert_eq!(
                decoded_resource.file.contents(),
                reloaded_resource.file.contents()
            );
            assert_eq!(
                decoded_resource.file.digest(),
                reloaded_resource.file.digest()
            );
        }
        assert_eq!(decoded.canonical_bytes().unwrap(), bytes);
        assert_eq!(decoded.digest().unwrap(), reloaded.digest().unwrap());
    }

    #[lyquid_test::test]
    fn parses_real_catalog_manifest() {
        let skill = load_doc_coauthoring();

        assert_eq!(skill.id, "doc-coauthoring");
        assert_eq!(skill.name, "doc-coauthoring");
        assert!(
            skill
                .description
                .starts_with("Guide users through a structured workflow")
        );
        assert!(
            skill
                .instructions
                .starts_with("\n# Doc Co-Authoring Workflow")
        );
        assert!(skill.resources.is_empty());
    }

    #[lyquid_test::test]
    fn loads_real_catalog_resources() {
        let skill = load_internal_comms();

        assert_eq!(skill.name, "internal-comms");
        assert_eq!(skill.id, skill.name);
        assert!(
            skill
                .resources
                .iter()
                .all(|r| !r.file.contents().is_empty())
        );

        let mut paths: Vec<_> = skill
            .resources
            .iter()
            .map(|resource| resource.path.as_str())
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "LICENSE.txt",
                "examples/3p-updates.md",
                "examples/company-newsletter.md",
                "examples/faq-answers.md",
                "examples/general-comms.md",
            ]
        );

        let mut sorted = skill.clone();
        sorted
            .resources
            .sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(skill.digest().unwrap(), sorted.digest().unwrap());
    }

    #[lyquid_test::test]
    fn canonical_bytes_round_trip_real_skills() {
        assert_real_skill_canonical_round_trip(load_doc_coauthoring(), load_doc_coauthoring());
        assert_real_skill_canonical_round_trip(load_internal_comms(), load_internal_comms());
    }

    #[lyquid_test::test]
    fn uses_name_as_id_when_absent() {
        let manifest = MarkdownFile::parse(
            TextFile::new(
                "---\nname: inline-skill\ndescription: An inline skill.\n---\n\nInline body.\n",
            )
            .unwrap(),
        )
        .unwrap();
        let skill = Skill::from_manifest(manifest, Vec::new());

        assert_eq!(skill.name, "inline-skill");
        assert_eq!(skill.id, skill.name);
    }

    #[lyquid_test::test]
    fn rejects_invalid_frontmatter() {
        let error =
            MarkdownFile::parse(TextFile::new("---\nname: [unterminated\n---\n\nBody.\n").unwrap())
                .unwrap_err();

        assert!(matches!(error, SkillError::InvalidFrontmatter(_)));
    }

    #[lyquid_test::test]
    fn canonical_bytes_sort_resources() {
        let mut sorted = internal_comms_resources();
        sorted.sort_by(|left, right| left.path.cmp(&right.path));
        let mut unsorted = sorted.clone();
        unsorted.reverse();

        let unsorted = Skill::from_manifest(internal_comms_manifest(), unsorted);
        let sorted = Skill::from_manifest(internal_comms_manifest(), sorted);

        assert_eq!(
            unsorted.canonical_bytes().unwrap(),
            sorted.canonical_bytes().unwrap()
        );
        assert_eq!(unsorted.digest().unwrap(), sorted.digest().unwrap());
    }

    #[lyquid_test::test]
    fn resource_path_affects_skill_digest() {
        let first = load_internal_comms();
        let mut second = first.clone();
        second.resources[0].path = "LICENSE.md".into();

        assert_ne!(first.digest().unwrap(), second.digest().unwrap());
    }

    #[lyquid_test::test]
    fn rejects_unknown_canonical_version() {
        let mut bytes = skill(Vec::new()).canonical_bytes().unwrap();
        bytes[0] = 2;

        assert!(Skill::from_canonical_bytes(&bytes).is_err());
    }

    #[lyquid_test::test]
    fn rejects_duplicate_resource_paths() {
        let duplicate = skill(vec![
            SkillResource::new("a.txt", "first").unwrap(),
            SkillResource::new("a.txt", "second").unwrap(),
        ]);

        assert!(duplicate.canonical_bytes().is_err());
    }

    #[lyquid_test::test]
    fn rejects_trailing_canonical_bytes() {
        let mut bytes = skill(Vec::new()).canonical_bytes().unwrap();
        bytes.push(0);

        assert!(Skill::from_canonical_bytes(&bytes).is_err());
    }

    #[lyquid_test::test]
    fn metadata_and_extra_affect_skill_digest() {
        let plain = skill(Vec::new());

        let mut with_metadata = plain.clone();
        with_metadata.metadata.insert("author".into(), "me".into());

        let mut with_extra = plain.clone();
        with_extra
            .extra
            .insert("custom".into(), serde_json::Value::String("value".into()));

        assert_ne!(plain.digest().unwrap(), with_metadata.digest().unwrap());
        assert_ne!(plain.digest().unwrap(), with_extra.digest().unwrap());
    }

    #[lyquid_test::test]
    fn canonical_bytes_round_trip_preserves_metadata_and_extra() {
        let mut original = skill(Vec::new());
        original.metadata.insert("author".into(), "me".into());
        original.extra.insert(
            "custom".into(),
            serde_json::json!({
                "enabled": true,
                "nested": [1, "two", null],
                "options": {"z": 2, "a": 1}
            }),
        );

        let bytes = original.canonical_bytes().unwrap();
        let decoded = Skill::from_canonical_bytes(&bytes).unwrap();

        assert_eq!(decoded, original);
    }

    #[lyquid_test::test]
    fn from_manifest_preserves_metadata_and_extra() {
        let source = "---\nname: demo\ndescription: A demo\nmetadata:\n  author: me\ncustom: value\n---\nBody";
        let manifest = MarkdownFile::parse(crate::skill::TextFile::new(source).unwrap()).unwrap();
        let skill = Skill::from_manifest(manifest, Vec::new());

        assert_eq!(skill.metadata.get("author").map(String::as_str), Some("me"));
        assert_eq!(
            skill.extra.get("custom").and_then(|value| value.as_str()),
            Some("value")
        );
    }
}
