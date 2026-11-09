"""Integration test for the full search lifecycle against the local devnet.

The devnet and ``tests/fixture/nodes.json`` are prepared by
``make prepare-nodes``. When the fixture is absent (plain unit-test run) the
module is skipped; when the fixture exists but the devnet is not reachable, the
test skips instead of failing.
"""

from __future__ import annotations

import importlib.util
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from types import ModuleType
from typing import Any

import httpx
import pytest
from mcp.types import EmbeddedResource, TextContent, TextResourceContents

from mead.node_manager import NodeManager
from mead.tools import MeadTools
from mead.utils import CLIArgs

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "search_lifecycle.py"
NODES_JSON = ROOT / "tests" / "fixture" / "nodes.json"

pytestmark = pytest.mark.skipif(
    not NODES_JSON.is_file(),
    reason="devnet fixture missing; run `make prepare-nodes` first",
)


def _load_lifecycle() -> ModuleType:
    spec = importlib.util.spec_from_file_location("search_lifecycle", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _devnet_reachable(lifecycle: ModuleType) -> bool:
    load_nodes = lifecycle.load_nodes  # type: ignore[attr-defined]
    get_json = lifecycle.get_json  # type: ignore[attr-defined]
    try:
        _, nodes = load_nodes(NODES_JSON)
        with httpx.Client() as client:
            status, _ = get_json(client, nodes[0], "/skills/count", timeout=2.0)
    except httpx.HTTPError, RuntimeError:
        return False
    return status == 200


def test_oracle_activation_is_idempotent_and_concurrent() -> None:
    lifecycle = _load_lifecycle()
    if not _devnet_reachable(lifecycle):
        pytest.skip("devnet is not reachable")

    _, nodes = lifecycle.load_nodes(NODES_JSON)  # type: ignore[attr-defined]
    node = nodes[0]
    headers = {"Host": node.host}
    activate_url = f"{node.base_url}/oracle/activate"
    status_url = f"{node.base_url}/oracle/status"

    def activate() -> tuple[int, dict[str, Any]]:
        response = httpx.post(activate_url, headers=headers, timeout=30.0)
        return response.status_code, response.json()

    # The devnet is already active from make prepare-nodes, so re-activation
    # must be a no-op that returns the active state immediately.
    for _ in range(2):
        status, body = activate()
        assert status == 200
        assert body["status"] == "active"

    # Concurrent calls once active are also no-ops.
    with ThreadPoolExecutor(max_workers=4) as pool:
        results = list(pool.map(lambda _: activate(), range(4)))
    assert all(status == 200 for status, _ in results)
    assert all(body["status"] == "active" for _, body in results)

    status_response = httpx.get(status_url, headers=headers, timeout=5.0)
    assert status_response.status_code == 200
    status_body = status_response.json()
    assert status_body["active"] is True
    assert status_body["source_active"] is True
    assert status_body["source_epoch"] == status_body["epoch"]


def test_search_lifecycle_completes() -> None:
    lifecycle = _load_lifecycle()
    if not _devnet_reachable(lifecycle):
        pytest.skip("devnet is not reachable")

    run_lifecycle = lifecycle.run_lifecycle  # type: ignore[attr-defined]
    last_error: RuntimeError | None = None

    # The first search after a fresh devnet can lose its peer replies while the
    # nodes warm up. Retry a couple of times with a fresh query.
    for attempt in range(3):
        try:
            result = run_lifecycle(
                nodes_json=NODES_JSON,
                query=f"skill integration {attempt}",
                timeout=60.0,
                poll_interval=1.0,
            )
        except RuntimeError as error:
            last_error = error
            time.sleep(2)
            continue

        status = result.status
        assert status["completed"] is True
        assert status["timed_out"] is False
        assert status["received_results"] == status["expected_results"]

        results = result.results
        assert results["completed"] is True
        assert results["timed_out"] is False
        assert results["rejected_results"] == 0
        assert results["verified_results"] == len(results["results"])
        # Peer results plus the initiator's local result.
        assert len(results["results"]) == status["received_results"] + 1
        assert isinstance(results["aggregated"], list)
        assert results["aggregated"]

        lyquid, nodes = lifecycle.load_nodes(NODES_JSON)  # type: ignore[attr-defined]
        manager = NodeManager(
            CLIArgs(
                server_url=nodes[0].base_url,
                skill_dir=ROOT / "tests" / "fixture" / "node1",
                node_id=nodes[0].node_id,
                lyquid_id=lyquid,
            )
        )
        local_digests = {
            skill.digest.removeprefix("0x").lower() for skill in manager.list_skills()
        }
        peer_digest = next(
            (
                match["skill_digest"]
                for match in results["aggregated"]
                if match["skill_digest"].removeprefix("0x").lower() not in local_digests
            ),
            None,
        )
        assert peer_digest is not None, "aggregated results contained no peer skill"

        peer_skill = manager.get_skill(peer_digest)
        assert peer_skill.name
        summaries = manager.search_skill_summaries(result.query_id)
        assert any(
            summary.digest.removeprefix("0x").lower()
            == peer_digest.removeprefix("0x").lower()
            for summary in summaries
        )

        blocks = MeadTools(manager).mcp_get_skill(peer_digest)
        summary = "\n".join(
            block.text for block in blocks if isinstance(block, TextContent)
        )
        assert peer_skill.name in summary
        embedded = [
            block.resource.text
            for block in blocks
            if isinstance(block, EmbeddedResource)
            and isinstance(block.resource, TextResourceContents)
        ]
        assert peer_skill.instructions in "\n".join(embedded)
        return

    pytest.fail(f"search lifecycle did not complete after retries: {last_error}")
