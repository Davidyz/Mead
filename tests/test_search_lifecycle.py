"""Integration test for the full search lifecycle against the local devnet.

The devnet and ``tests/fixture/nodes.json`` are prepared by
``make prepare-nodes``. When the fixture is absent (plain unit-test run) the
module is skipped; when the fixture exists but the devnet is not reachable, the
test skips instead of failing.
"""

from __future__ import annotations

import importlib.util
import time
from pathlib import Path
from types import ModuleType

import httpx
import pytest

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
        return

    pytest.fail(f"search lifecycle did not complete after retries: {last_error}")
