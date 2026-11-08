import json
import time
from pathlib import Path
from typing import Any, cast

import httpx
import pytest

from mead.node_manager import NodeManager, SkillSummary
from mead.skills import Skill
from mead.tools import MeadTools
from mead.utils import CLIArgs

FIXTURE_DIR = Path(__file__).parent / "fixture"
NODES_JSON = FIXTURE_DIR / "nodes.json"


class FakeNodeManager:
    def __init__(self, summaries: list[SkillSummary], skills: dict[str, Skill]):
        self.summaries = summaries
        self.skills = skills
        self.search_calls: list[tuple[str, int]] = []
        self.get_calls: list[str] = []

    def list_skills(self) -> list[SkillSummary]:
        return list(self.summaries)

    def search_skills(self, query: str, top_k: int = 5) -> list[SkillSummary]:
        self.search_calls.append((query, top_k))
        return list(self.summaries)

    def get_skill(self, digest: str) -> Skill:
        self.get_calls.append(digest)
        return self.skills[digest]


def test_mcp_list_skill_returns_names_descriptions_and_digests() -> None:
    summaries = [
        SkillSummary("zeta", "The last skill", "digest-z"),
        SkillSummary("alpha", "The first skill", "digest-a"),
    ]
    tools = MeadTools(cast(NodeManager, FakeNodeManager(summaries, {})))

    assert tools.mcp_list_skill() == [
        {"name": "alpha", "description": "The first skill", "digest": "digest-a"},
        {"name": "zeta", "description": "The last skill", "digest": "digest-z"},
    ]


def test_mcp_search_skills_passes_query_and_top_k() -> None:
    summaries = [SkillSummary("demo", "A demo skill", "digest-demo")]
    fake = FakeNodeManager(summaries, {})
    tools = MeadTools(cast(NodeManager, fake))

    assert tools.mcp_search_skills("demo query", top_k=3) == [
        {"name": "demo", "description": "A demo skill", "digest": "digest-demo"}
    ]
    assert fake.search_calls == [("demo query", 3)]


def test_mcp_get_skill_exposes_description_or_selected_resources() -> None:
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

    assert tools.mcp_get_skill("digest-demo", "description") == "A demo skill"

    full = tools.mcp_get_skill("digest-demo", "full", ["nested/b.txt"])
    assert "# demo" in full
    assert "Use this skill." in full
    assert "## Resource: nested/b.txt\n\nB" in full
    assert "## Resource: a.txt" not in full
    assert "- a.txt" in full
    assert fake.get_calls == ["digest-demo", "digest-demo"]


def test_mcp_get_skill_rejects_unknown_resources_and_detail_levels() -> None:
    skill = Skill.from_manifest(
        {"name": "demo", "description": "A demo skill"},
        "Body",
        [{"path": "a.txt", "contents": "A"}],
    )
    tools = MeadTools(cast(NodeManager, FakeNodeManager([], {"digest-demo": skill})))

    with pytest.raises(ValueError, match="does not contain resources: missing.txt"):
        tools.mcp_get_skill("digest-demo", "full", ["missing.txt"])
    with pytest.raises(ValueError, match="unsupported detail level"):
        tools.mcp_get_skill("digest-demo", cast(Any, "summary"))


@pytest.fixture(scope="session")
def devnet() -> dict[str, Any]:
    if not NODES_JSON.is_file():
        pytest.skip("devnet fixture is missing; run `make prepare-nodes` first")

    data = json.loads(NODES_JSON.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("nodes"), list):
        raise TypeError(f"{NODES_JSON} does not contain a node list")
    return data


def node_manager(devnet: dict[str, Any]) -> NodeManager:
    node = next(node for node in devnet["nodes"] if node["index"] == 1)
    return NodeManager(
        CLIArgs(
            server_url=f"http://127.0.0.1:{node['port']}",
            skill_dir=FIXTURE_DIR / "node1",
            node_id=node["node_id"],
            lyquid_id=devnet["lyquid_id"],
        )
    )


def test_search_and_get_skill_round_trip(devnet: dict[str, Any]) -> None:
    manager = node_manager(devnet)
    tools = MeadTools(manager)
    last_error: Exception | None = None

    # The first search after a fresh devnet can lose its peer replies while the
    # nodes warm up; retry with a fresh query.
    for attempt in range(3):
        try:
            summaries = tools.mcp_search_skills(f"skill tools {attempt}", top_k=5)
        except (httpx.HTTPError, RuntimeError) as error:
            last_error = error
            time.sleep(2)
            continue

        if not summaries:
            last_error = RuntimeError("search returned no skills")
            time.sleep(2)
            continue

        summary = summaries[0]
        skill = manager.get_skill(summary["digest"])
        assert skill.name == summary["name"]
        assert skill.description == summary["description"]

        description = tools.mcp_get_skill(summary["digest"], "description")
        assert description == skill.description

        full = tools.mcp_get_skill(summary["digest"], "full")
        assert f"# {skill.name}" in full
        assert skill.instructions in full
        return

    pytest.fail(f"MCP search/get round trip failed: {last_error}")
