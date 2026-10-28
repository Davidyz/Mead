import json
from pathlib import Path

import pytest

from mead.skills import Frontmatter, Skill, SkillManager, SkillResource

FIXTURE_SKILLS_DIR = Path(__file__).parent / "skills" / "skills"


@pytest.fixture(scope="session")
def fixture_skills_dir() -> Path:
    if not FIXTURE_SKILLS_DIR.is_dir():
        pytest.skip("skill fixtures are missing; run `make prepare` first")
    return FIXTURE_SKILLS_DIR


def write_skill(
    skill_dir: Path,
    source: str,
    *,
    resources: dict[str, str] | None = None,
) -> Path:
    skill_dir.mkdir(parents=True, exist_ok=True)
    (skill_dir / "SKILL.md").write_text(source, encoding="utf-8")
    for path, contents in (resources or {}).items():
        resource = skill_dir / path
        resource.parent.mkdir(parents=True, exist_ok=True)
        resource.write_text(contents, encoding="utf-8")
    return skill_dir


def test_parses_real_catalog_manifest(fixture_skills_dir: Path) -> None:
    skill = Skill(fixture_skills_dir / "doc-coauthoring")

    assert skill.id == "doc-coauthoring"
    assert skill.name == "doc-coauthoring"
    assert skill.description.startswith("Guide users through a structured workflow")
    assert skill.instructions.startswith("\n# Doc Co-Authoring Workflow")
    assert skill.triggers == []
    assert skill.capabilities == []
    assert skill.metadata == {}
    assert skill.extra == {}
    assert skill.resources == []


def test_loads_real_catalog_resources(fixture_skills_dir: Path) -> None:
    skill = Skill(fixture_skills_dir / "internal-comms")

    assert skill.id == skill.name == "internal-comms"
    assert skill.instructions.startswith("\n## When to use this skill")
    assert skill.extra == {"license": "Complete terms in LICENSE.txt"}
    assert [resource.path for resource in skill.resources] == [
        "LICENSE.txt",
        "examples/3p-updates.md",
        "examples/company-newsletter.md",
        "examples/faq-answers.md",
        "examples/general-comms.md",
    ]
    assert all(resource.contents for resource in skill.resources)


def test_real_skills_round_trip_through_json(fixture_skills_dir: Path) -> None:
    for name in ("doc-coauthoring", "internal-comms"):
        skill = Skill(fixture_skills_dir / name)
        decoded = Skill.from_json(skill.to_json())

        assert decoded.to_dict() == skill.to_dict()
        assert decoded.id == skill.id
        assert decoded.instructions == skill.instructions
        assert decoded.extra == skill.extra
        assert decoded.resources == skill.resources


def test_json_schema_matches_rust_transport(fixture_skills_dir: Path) -> None:
    data = Skill(fixture_skills_dir / "internal-comms").to_dict()

    assert list(data) == [
        "id",
        "name",
        "description",
        "triggers",
        "instructions",
        "capabilities",
        "metadata",
        "extra",
        "resources",
    ]
    assert data["resources"][0] == {
        "path": "LICENSE.txt",
        "contents": (fixture_skills_dir / "internal-comms" / "LICENSE.txt").read_text(
            encoding="utf-8"
        ),
    }


def test_json_uses_name_as_id_when_absent() -> None:
    skill = Skill.from_json(
        json.dumps(
            {
                "name": "json-skill",
                "description": "A JSON skill.",
                "instructions": "Use it.",
                "resources": [],
            }
        )
    )

    assert skill.id == skill.name == "json-skill"


def test_json_defaults_optional_fields() -> None:
    skill = Skill.from_json(
        '{"name":"json-skill","description":"A JSON skill.","instructions":"Use it."}'
    )

    assert skill.id == "json-skill"
    assert skill.triggers == []
    assert skill.capabilities == []
    assert skill.metadata == {}
    assert skill.extra == {}
    assert skill.resources == []


def test_json_preserves_metadata_extra_and_resources() -> None:
    source = {
        "id": "custom-id",
        "name": "json-skill",
        "description": "A JSON skill.",
        "triggers": ["one", "two"],
        "instructions": "Use it.",
        "capabilities": ["testing"],
        "metadata": {"author": "me"},
        "extra": {
            "custom": {
                "enabled": True,
                "nested": [1, "two", None],
            }
        },
        "resources": [{"path": "examples/example.md", "contents": "Example."}],
    }

    skill = Skill.from_json(json.dumps(source))

    assert skill.id == "custom-id"
    assert skill.triggers == ["one", "two"]
    assert skill.capabilities == ["testing"]
    assert skill.metadata == {"author": "me"}
    assert skill.extra == source["extra"]
    assert skill.resources == [
        SkillResource(path="examples/example.md", contents="Example.")
    ]


@pytest.mark.parametrize(
    "payload",
    [
        "{",
        "[]",
        '{"name":"json-skill","description":"A JSON skill.","instructions":"Use it.","resources":[{"path":"../escape","contents":"nope"}]}',
        '{"name":"json-skill","description":"A JSON skill.","instructions":"Use it.","resources":[{"path":"a.txt","contents":"first"},{"path":"a.txt","contents":"second"}]}',
        '{"name":"json-skill","description":"A JSON skill.","instructions":"Use it.","resources":[{"path":"/absolute","contents":"nope"}]}',
        '{"name":"json-skill","description":"A JSON skill.","instructions":"Use it.","resources":[{"path":"a\\\\b","contents":"nope"}]}',
        '{"name":"json-skill","description":"A JSON skill.","instructions":"Use it.","triggers":"not-a-list"}',
        '{"name":"json-skill","description":"A JSON skill.","instructions":"Use it.","metadata":{"author":1}}',
    ],
)
def test_json_rejects_invalid_skills(payload: str) -> None:
    with pytest.raises((TypeError, ValueError)):
        Skill.from_json(payload)


def test_parses_triggers_capabilities_and_preserves_body(
    tmp_path: Path,
) -> None:
    source = (
        "---\n"
        "name: inline-skill\n"
        "description: An inline skill.\n"
        "triggers:\n"
        "  - inline\n"
        "capabilities:\n"
        "  - testing\n"
        "---\n"
        "\n"
        "# Inline body\n"
    )
    skill = Skill(write_skill(tmp_path / "skill", source))

    assert skill.instructions == "\n# Inline body\n"
    assert skill.triggers == ["inline"]
    assert skill.capabilities == ["testing"]


def test_uses_name_as_id_when_blank(tmp_path: Path) -> None:
    source = (
        "---\nid: '  '\nname: inline-skill\ndescription: An inline skill.\n---\nBody\n"
    )
    skill = Skill(write_skill(tmp_path / "skill", source))

    assert skill.id == skill.name == "inline-skill"


def test_preserves_metadata_and_unknown_frontmatter(tmp_path: Path) -> None:
    source = (
        "---\n"
        "name: demo\n"
        "description: A demo\n"
        "metadata:\n"
        "  author: me\n"
        "custom: value\n"
        "---\n"
        "Body"
    )
    skill = Skill(write_skill(tmp_path / "skill", source))

    assert skill.metadata == {"author": "me"}
    assert skill.extra == {"custom": "value"}


def test_parse_frontmatter_returns_metadata(tmp_path: Path) -> None:
    source = "---\nname: demo\ndescription: A demo\n---\nBody"
    skill = Skill(write_skill(tmp_path / "skill", source))

    parsed = skill.parse_frontmatter()

    assert isinstance(parsed, Frontmatter)
    assert parsed.name == "demo"
    assert parsed.description == "A demo"
    assert skill.name == "demo"


def test_load_resources_recursively_and_skips_hidden_files(tmp_path: Path) -> None:
    source = "---\nname: demo\ndescription: A demo\n---\nBody"
    skill_dir = write_skill(
        tmp_path / "skill",
        source,
        resources={
            "nested/reference.md": "Reference.",
            "scripts/run.py": "print('hi')\n",
            ".hidden/ignored.txt": "ignored",
            ".hidden-file.txt": "ignored",
        },
    )

    skill = Skill(skill_dir)

    assert skill.resources == [
        SkillResource(path="nested/reference.md", contents="Reference."),
        SkillResource(path="scripts/run.py", contents="print('hi')\n"),
    ]


def test_from_manifest_flattens_unknown_frontmatter() -> None:
    skill = Skill.from_manifest(
        {
            "name": "demo",
            "description": "A demo",
            "custom": "value",
        },
        "Body",
        [{"path": "example.md", "contents": "Example."}],
    )

    assert skill.id == "demo"
    assert skill.extra == {"custom": "value"}
    assert skill.resources == [SkillResource(path="example.md", contents="Example.")]


@pytest.mark.parametrize(
    "source",
    [
        "Body without frontmatter",
        "---\nname: demo\ndescription: A demo\nBody without closing delimiter",
        "---\nname: demo\n---\nBody missing description",
        "---\ndescription: A demo\n---\nBody missing name",
    ],
)
def test_rejects_invalid_frontmatter(tmp_path: Path, source: str) -> None:
    with pytest.raises(ValueError):
        Skill(write_skill(tmp_path / "skill", source))


def test_requires_skill_md(tmp_path: Path) -> None:
    skill_dir = tmp_path / "not-a-skill"
    skill_dir.mkdir()

    with pytest.raises(ValueError, match="does not contain a SKILL.md"):
        Skill(skill_dir)


def test_canonical_key_ignores_unordered_collections() -> None:
    def build(
        triggers: list[str],
        capabilities: list[str],
        metadata: dict[str, str],
        extra: dict[str, object],
        resources: list[dict[str, str]],
    ) -> Skill:
        return Skill.from_manifest(
            {
                "id": "demo",
                "name": "demo",
                "description": "A demo",
                "triggers": triggers,
                "capabilities": capabilities,
                "metadata": metadata,
                "extra": extra,
            },
            "Body",
            resources,
        )

    first = build(
        ["beta", "alpha"],
        ["writing", "testing"],
        {"z": "last", "a": "first"},
        {"nested": {"z": 2, "a": 1}},
        [
            {"path": "b.txt", "contents": "B"},
            {"path": "a.txt", "contents": "A"},
        ],
    )
    reordered = build(
        ["alpha", "beta"],
        ["testing", "writing"],
        {"a": "first", "z": "last"},
        {"nested": {"a": 1, "z": 2}},
        [
            {"path": "a.txt", "contents": "A"},
            {"path": "b.txt", "contents": "B"},
        ],
    )

    assert first.canonical_key() == reordered.canonical_key()


def test_canonical_key_preserves_list_multiplicity() -> None:
    first = Skill.from_manifest(
        {"name": "demo", "description": "A demo", "triggers": ["alpha"]}, "Body"
    )
    repeated = Skill.from_manifest(
        {"name": "demo", "description": "A demo", "triggers": ["alpha", "alpha"]},
        "Body",
    )

    assert first.canonical_key() != repeated.canonical_key()


def test_manager_loads_fixture_catalog(fixture_skills_dir: Path) -> None:
    manager = SkillManager(fixture_skills_dir.parent)
    manager.load_skills()

    assert manager._skills
    assert all(isinstance(skill, Skill) for skill in manager._skills.values())
    assert set(manager._skills) == {
        skill.canonical_key() for skill in manager._skills.values()
    }
    assert "doc-coauthoring" in {skill.id for skill in manager._skills.values()}
    assert "internal-comms" in {skill.id for skill in manager._skills.values()}


def test_manager_skips_skills_with_non_text_resources(tmp_path: Path) -> None:
    source = "---\nname: demo\ndescription: A demo\n---\nBody"
    write_skill(tmp_path / "valid", source)
    invalid_dir = write_skill(tmp_path / "invalid", source)
    (invalid_dir / "asset.bin").write_bytes(b"\xff\xfe")

    manager = SkillManager(tmp_path)
    manager.load_skills()

    assert [skill.id for skill in manager._skills.values()] == ["demo"]


def test_manager_holds_in_memory_skills_without_a_directory() -> None:
    skill = Skill.from_manifest({"name": "demo", "description": "A demo"}, "Body")
    manager = SkillManager()

    manager.add_skill(skill)

    assert manager._skills == {skill.canonical_key(): skill}


def test_manager_deduplicates_canonically_equal_skills() -> None:
    first = Skill.from_manifest(
        {
            "name": "demo",
            "description": "A demo",
            "triggers": ["beta", "alpha"],
        },
        "Body",
    )
    reordered = Skill.from_manifest(
        {
            "name": "demo",
            "description": "A demo",
            "triggers": ["alpha", "beta"],
        },
        "Body",
    )
    manager = SkillManager()

    manager.add_skill(first)
    manager.add_skill(reordered)

    assert list(manager._skills.values()) == [first]
