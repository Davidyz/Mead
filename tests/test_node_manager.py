from pathlib import Path

import httpx
import pytest

from mead import node_manager
from mead.skills import Skill
from mead.utils import CLIArgs


@pytest.fixture
def cli_args() -> CLIArgs:
    return CLIArgs(
        server_url="http://127.0.0.1:10087/",
        skill_dir=Path("."),
        node_id="Node-node-label",
        lyquid_id="Lyquid-lyquid-label",
    )


def test_list_skills_sends_virtual_host_and_returns_dataclasses(
    cli_args: CLIArgs, monkeypatch: pytest.MonkeyPatch
) -> None:
    calls: list[tuple[str, dict[str, str]]] = []

    def fake_get(url: str, *, headers: dict[str, str]) -> httpx.Response:
        calls.append((url, headers))
        return httpx.Response(
            200,
            json=[
                {
                    "name": "demo",
                    "description": "A demo skill.",
                    "digest": "abc",
                }
            ],
        )

    monkeypatch.setattr(node_manager.httpx, "get", fake_get)

    skills = node_manager.NodeManager(cli_args).list_skills()

    assert skills == [
        node_manager.SkillSummary(
            name="demo", description="A demo skill.", digest="abc"
        )
    ]
    assert calls == [
        (
            "http://127.0.0.1:10087/skills/list",
            {
                "accept": "application/json",
                "Host": "lyquid-label.node-label",
            },
        )
    ]


def test_upload_skill_sends_transport_json_and_returns_dataclasses(
    cli_args: CLIArgs, monkeypatch: pytest.MonkeyPatch
) -> None:
    calls: list[tuple[str, dict[str, str], dict[str, object]]] = []
    skill = Skill.from_manifest(
        {"name": "demo", "description": "A demo skill."}, "Use it."
    )

    def fake_post(
        url: str, *, headers: dict[str, str], json: dict[str, object]
    ) -> httpx.Response:
        calls.append((url, headers, json))
        return httpx.Response(200, json={"uploaded": 1, "digests": ["abc"]})

    monkeypatch.setattr(node_manager.httpx, "post", fake_post)

    response = node_manager.NodeManager(cli_args).upload_skill(skill)

    assert response == node_manager.UploadSkillResponse(uploaded=1, digests=["abc"])
    assert calls == [
        (
            "http://127.0.0.1:10087/skills/upload",
            {
                "accept": "application/json",
                "content-type": "application/json",
                "Host": "lyquid-label.node-label",
            },
            skill.to_dict(),
        )
    ]
