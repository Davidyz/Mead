from typing import Any, cast

import pytest
from mcp.types import EmbeddedResource, TextContent, TextResourceContents

from mead.node_manager import NodeManager, SearchOutcome, SkillSummary
from mead.skills import Skill
from mead.tools import MeadTools


def _embedded_text(block: EmbeddedResource) -> str:
    assert isinstance(block.resource, TextResourceContents)
    return block.resource.text


class FakeNodeManager:
    def __init__(
        self,
        summaries: list[SkillSummary],
        skills: dict[str, Skill],
        *,
        timed_out: bool = False,
        unresolved_digests: list[str] | None = None,
    ):
        self.summaries = summaries
        self.skills = skills
        self.timed_out = timed_out
        self.unresolved_digests = unresolved_digests or []
        self.search_calls: list[tuple[str, int]] = []
        self.get_calls: list[str] = []

    def search_skills(self, query: str, top_k: int = 5) -> SearchOutcome:
        self.search_calls.append((query, top_k))
        return SearchOutcome(
            skills=list(self.summaries),
            timed_out=self.timed_out,
            received_results=len(self.summaries),
            expected_results=len(self.summaries),
            verified_results=len(self.summaries),
            rejected_results=0,
            unresolved_digests=list(self.unresolved_digests),
        )

    def get_skill(self, digest: str) -> Skill:
        self.get_calls.append(digest)
        return self.skills[digest]


def test_mcp_search_skills_passes_query_and_top_k() -> None:
    summaries = [SkillSummary("demo", "A demo skill", "digest-demo")]
    fake = FakeNodeManager(summaries, {})
    tools = MeadTools(cast(NodeManager, fake))

    assert tools.mcp_search_skills("demo query", top_k=3) == {
        "skills": [
            {"name": "demo", "description": "A demo skill", "digest": "digest-demo"}
        ],
        "timed_out": False,
        "received_results": 1,
        "expected_results": 1,
        "verified_results": 1,
        "rejected_results": 0,
        "unresolved_digests": [],
    }
    assert fake.search_calls == [("demo query", 3)]


def test_mcp_search_skills_surfaces_partial_result_metadata() -> None:
    summary = SkillSummary("demo", "A demo skill", "digest-demo")
    fake = FakeNodeManager(
        [summary], {}, timed_out=True, unresolved_digests=["digest-missing"]
    )
    tools = MeadTools(cast(NodeManager, fake))

    assert tools.mcp_search_skills("demo query") == {
        "skills": [
            {"name": "demo", "description": "A demo skill", "digest": "digest-demo"}
        ],
        "timed_out": True,
        "received_results": 1,
        "expected_results": 1,
        "verified_results": 1,
        "rejected_results": 0,
        "unresolved_digests": ["digest-missing"],
    }


def test_mcp_get_skill_exposes_skill_md_or_selected_resources() -> None:
    skill = Skill.from_manifest(
        {
            "id": "custom-id",
            "name": "demo",
            "description": "A demo skill",
            "triggers": ["demo"],
        },
        "Use this skill.",
        [
            {"path": "a.txt", "contents": "A"},
            {"path": "nested/b.txt", "contents": "B"},
        ],
    )
    fake = FakeNodeManager([], {"digest-demo": skill})
    tools = MeadTools(cast(NodeManager, fake))

    skill_md = tools.mcp_get_skill("digest-demo")
    summary = next(block for block in skill_md if isinstance(block, TextContent))
    assert "Skill: demo" in summary.text
    assert "Description: A demo skill" in summary.text
    assert "- a.txt" in summary.text
    assert "- nested/b.txt" in summary.text
    embedded = [block for block in skill_md if isinstance(block, EmbeddedResource)]
    assert len(embedded) == 1
    assert embedded[0].resource.uri == "skill://digest-demo/SKILL.md"
    assert _embedded_text(embedded[0]) == "Use this skill."

    selected = tools.mcp_get_skill("digest-demo", paths=["nested/b.txt"])
    selected_embedded = [
        block for block in selected if isinstance(block, EmbeddedResource)
    ]
    assert [block.resource.uri for block in selected_embedded] == [
        "skill://digest-demo/SKILL.md",
        "skill://digest-demo/nested/b.txt",
    ]
    assert _embedded_text(selected_embedded[1]) == "B"

    full = tools.mcp_get_skill("digest-demo", "full")
    full_embedded = [block for block in full if isinstance(block, EmbeddedResource)]
    assert [block.resource.uri for block in full_embedded] == [
        "skill://digest-demo/SKILL.md",
        "skill://digest-demo/a.txt",
        "skill://digest-demo/nested/b.txt",
    ]
    assert [_embedded_text(block) for block in full_embedded] == [
        "Use this skill.",
        "A",
        "B",
    ]
    assert fake.get_calls == ["digest-demo", "digest-demo", "digest-demo"]


def test_mcp_get_skill_rejects_unknown_resources_and_detail_levels() -> None:
    skill = Skill.from_manifest(
        {"name": "demo", "description": "A demo skill"},
        "Body",
        [{"path": "a.txt", "contents": "A"}],
    )
    tools = MeadTools(cast(NodeManager, FakeNodeManager([], {"digest-demo": skill})))

    with pytest.raises(ValueError, match="does not contain resources: missing.txt"):
        tools.mcp_get_skill("digest-demo", "skill_md", ["missing.txt"])
    with pytest.raises(ValueError, match="unsupported detail level"):
        tools.mcp_get_skill("digest-demo", cast(Any, "summary"))
