//! Shared test fixtures loaded from the prepared real skill catalog.
//!
//! Guest tests cannot access the filesystem, so catalog files are embedded at
//! compile time. Run `make prepare` before compiling tests.

use super::{MarkdownFile, Skill, SkillResource, TextFile};

pub fn load_doc_coauthoring() -> Skill {
    let manifest = MarkdownFile::parse(
        TextFile::new(include_str!(
            "../../tests/skills/skills/doc-coauthoring/SKILL.md"
        ))
        .unwrap(),
    )
    .unwrap();
    Skill::from_manifest(manifest, Vec::new())
}

pub fn load_internal_comms() -> Skill {
    let mut resources = internal_comms_resources();
    resources.sort_by(|left, right| left.path.cmp(&right.path));

    Skill::from_manifest(internal_comms_manifest(), resources)
}

pub fn internal_comms_manifest() -> MarkdownFile {
    MarkdownFile::parse(
        TextFile::new(include_str!(
            "../../tests/skills/skills/internal-comms/SKILL.md"
        ))
        .unwrap(),
    )
    .unwrap()
}

pub fn internal_comms_resources() -> Vec<SkillResource> {
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
            include_str!("../../tests/skills/skills/internal-comms/examples/company-newsletter.md"),
        )
        .unwrap(),
        SkillResource::new(
            "examples/faq-answers.md",
            include_str!("../../tests/skills/skills/internal-comms/examples/faq-answers.md"),
        )
        .unwrap(),
    ]
}
