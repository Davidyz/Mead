from __future__ import annotations

import asyncio
import json
import time
from pathlib import Path
from typing import Any, cast

import httpx
import pytest
from mcp.types import CallToolResult

from mead.node_manager import NodeManager
from mead.tools import MeadTools
from mead.utils import CLIArgs

FIXTURE_DIR = Path(__file__).parent / "fixture"
NODES_JSON = FIXTURE_DIR / "nodes.json"


class ShortTimeoutManager:
    """NodeManager wrapper that keeps the live test from waiting a full minute."""

    def __init__(self, manager: NodeManager):
        self._manager = manager

    def search_skills(self, query: str, top_k: int = 5):
        return self._manager.search_skills(
            query,
            top_k=top_k,
            timeout=15.0,
            poll_interval=0.5,
        )


def _devnet() -> dict[str, Any]:
    if not NODES_JSON.is_file():
        pytest.skip("devnet fixture is missing; run `make prepare-nodes` first")

    data = json.loads(NODES_JSON.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("nodes"), list):
        raise TypeError(f"{NODES_JSON} does not contain a node list")
    return data


def test_concurrent_mcp_searches_on_the_same_node_complete() -> None:
    """Two MCP search tool calls issued together must both complete.

    Search requests share the node's UPC machinery. Issuing a second search
    before the first announce step has run must not leave either query pending.
    """
    devnet = _devnet()
    node = devnet["nodes"][-1]
    manager = NodeManager(
        CLIArgs(
            server_url=f"http://127.0.0.1:{node['port']}",
            skill_dir=None,
            node_id=node["node_id"],
            lyquid_id=devnet["lyquid_id"],
        )
    )
    try:
        manager.list_skills()
    except httpx.TransportError as error:
        pytest.skip(f"devnet node4 is not available: {error}")

    server = MeadTools(
        cast(NodeManager, ShortTimeoutManager(manager))
    ).make_mcp_server()

    async def search(query: str):
        return await server.call_tool(
            "search_skills",
            {"query": query, "top_k": 5},
        )

    async def run_round(attempt: int) -> list[BaseException]:
        stamp = time.time_ns()
        results = await asyncio.gather(
            search(f"concurrent mcp {stamp} {attempt} 0"),
            search(f"concurrent mcp {stamp} {attempt} 1"),
            return_exceptions=True,
        )
        errors = [result for result in results if isinstance(result, BaseException)]
        if errors:
            return errors

        successes = [cast(CallToolResult, result) for result in results]
        assert len(successes) == 2
        assert all(not result.is_error for result in successes)
        return []

    async def run_with_retries() -> None:
        last_errors: list[BaseException] = []
        for attempt in range(3):
            last_errors = await run_round(attempt)
            if not last_errors:
                return
            await asyncio.sleep(2)
        raise AssertionError(f"concurrent MCP searches failed: {last_errors!r}")

    asyncio.run(run_with_retries())
