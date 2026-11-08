import json
from pathlib import Path
from typing import Any

import httpx
import pytest

from mead.node_manager import NodeManager, SkillSummary, UploadSkillResponse
from mead.skills import Skill
from mead.utils import CLIArgs

FIXTURE_DIR = Path(__file__).parent / "fixture"
NODES_JSON = FIXTURE_DIR / "nodes.json"
NODE_INDEXES = [1, 2, 3, 4]


@pytest.fixture(scope="session")
def devnet() -> dict[str, Any]:
    if not NODES_JSON.is_file():
        pytest.skip("devnet fixture is missing; run `make prepare-nodes` first")

    data = json.loads(NODES_JSON.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("nodes"), list):
        raise TypeError(f"{NODES_JSON} does not contain a node list")
    return data


def node_manager(
    devnet: dict[str, Any], index: int
) -> tuple[NodeManager, dict[str, Any]]:
    node = next(node for node in devnet["nodes"] if node["index"] == index)
    return (
        NodeManager(
            CLIArgs(
                server_url=f"http://127.0.0.1:{node['port']}",
                skill_dir=FIXTURE_DIR / f"node{index}",
                node_id=node["node_id"],
                lyquid_id=devnet["lyquid_id"],
            )
        ),
        node,
    )


@pytest.mark.parametrize("index", NODE_INDEXES)
def test_list_skills_returns_live_node_skills(
    devnet: dict[str, Any], index: int
) -> None:
    manager, node = node_manager(devnet, index)

    try:
        skills = manager.list_skills()
    except httpx.TransportError as error:
        pytest.skip(f"devnet node{index} is not available: {error}")

    assert skills
    assert all(isinstance(skill, SkillSummary) for skill in skills)
    assert all(skill.digest for skill in skills)
    assert {skill.name for skill in skills} == set(node["skills"])


@pytest.mark.parametrize("index", NODE_INDEXES)
def test_upload_skill_round_trips_through_live_node(
    devnet: dict[str, Any], index: int
) -> None:
    manager, _ = node_manager(devnet, index)
    skill_dirs = sorted(
        path.parent for path in (FIXTURE_DIR / f"node{index}").glob("*/SKILL.md")
    )
    if not skill_dirs:
        pytest.skip(f"node{index} fixture skills are missing")

    skill = Skill(skill_dirs[0])
    try:
        response = manager.upload_skill(skill)
        skills = manager.list_skills()
    except httpx.TransportError as error:
        pytest.skip(f"devnet node{index} is not available: {error}")

    assert isinstance(response, UploadSkillResponse)
    assert response.uploaded == 1
    assert len(response.digests) == 1
    assert any(
        candidate.name == skill.name and candidate.digest == response.digests[0]
        for candidate in skills
    )


def test_oracle_status_is_active(devnet: dict[str, Any]) -> None:
    manager, _ = node_manager(devnet, 1)

    try:
        status = manager.get_oracle_status()
    except httpx.TransportError as error:
        pytest.skip(f"devnet is not available: {error}")

    assert status.active is True
    assert status.epoch >= 1
    assert status.source_active is True
    assert status.source_epoch == status.epoch


def test_activate_oracle_is_idempotent(devnet: dict[str, Any]) -> None:
    manager, _ = node_manager(devnet, 1)

    try:
        first = manager.activate_oracle()
        second = manager.activate_oracle()
    except httpx.TransportError as error:
        pytest.skip(f"devnet is not available: {error}")

    assert first.status == "active"
    assert second.status == "active"


def test_register_node_is_idempotent(devnet: dict[str, Any]) -> None:
    manager, node = node_manager(devnet, 1)

    try:
        first = manager.register_node()
        second = manager.register_node()
        nodes = manager.list_nodes()
    except httpx.TransportError as error:
        pytest.skip(f"devnet is not available: {error}")

    assert first.status in {"registered", "submitted"}
    assert second.status in {"registered", "submitted"}
    assert node["node_id"] in nodes


def test_prepare_is_idempotent(devnet: dict[str, Any]) -> None:
    manager, node = node_manager(devnet, 1)

    try:
        result = manager.prepare(activate_oracle=True, timeout=30.0, poll_interval=0.5)
    except httpx.TransportError as error:
        pytest.skip(f"devnet is not available: {error}")

    assert result.node_id == node["node_id"]
    assert result.oracle.active is True
    assert result.node_id in result.nodes
