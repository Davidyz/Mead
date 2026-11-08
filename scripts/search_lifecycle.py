#!/usr/bin/env python3
"""Drive a complete Mead search against the local multi-node devnet.

This is both a development helper and the driver for the integration test in
``tests/test_search_lifecycle.py``. It deliberately uses only the query ID
returned by ``/search/new`` for the status and result requests.

Typical use:

    uv run python scripts/search_lifecycle.py --query "skill"

The lifecycle is:

1. POST /search/new on the initiator node.
2. Wait until the certified query call has registered the query.
3. Poll /search/status using the returned query ID.
4. POST /search/get and return the collected results.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import httpx

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_NODES_JSON = ROOT / "tests" / "fixture" / "nodes.json"


def strip_prefix(value: str, prefix: str) -> str:
    return value.removeprefix(prefix)


def virtual_host(lyquid_id: str, node_id: str) -> str:
    return f"{strip_prefix(lyquid_id, 'Lyquid-')}.{strip_prefix(node_id, 'Node-')}"


@dataclass(frozen=True)
class Node:
    index: int
    node_id: str
    port: int
    lyquid_id: str

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    @property
    def host(self) -> str:
        return virtual_host(self.lyquid_id, self.node_id)


@dataclass(frozen=True)
class LifecycleResult:
    query_id: str
    status: dict[str, Any]
    results: dict[str, Any]


def load_nodes(path: Path) -> tuple[str, list[Node]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    lyquid_id = data["lyquid_id"]
    nodes = [
        Node(
            index=int(entry["index"]),
            node_id=entry["node_id"],
            port=int(entry["port"]),
            lyquid_id=lyquid_id,
        )
        for entry in data["nodes"]
    ]
    nodes.sort(key=lambda node: node.index)
    if not nodes:
        raise RuntimeError(f"{path} does not contain any nodes")
    return lyquid_id, nodes


def post_json(
    client: httpx.Client, node: Node, path: str, payload: dict[str, Any], timeout: float
) -> tuple[int, Any]:
    response = client.post(
        f"{node.base_url}{path}",
        headers={"Host": node.host, "content-type": "application/json"},
        json=payload,
        timeout=timeout,
    )
    try:
        body = response.json()
    except ValueError:
        body = response.text
    return response.status_code, body


def get_json(
    client: httpx.Client, node: Node, path: str, timeout: float = 30.0
) -> tuple[int, Any]:
    response = client.get(
        f"{node.base_url}{path}",
        headers={"Host": node.host},
        timeout=timeout,
    )
    try:
        body = response.json()
    except ValueError:
        body = response.text
    return response.status_code, body


def load_skill_names(client: httpx.Client, nodes: list[Node]) -> dict[str, str]:
    names: dict[str, str] = {}
    for node in nodes:
        status, body = get_json(client, node, "/skills/list")
        if status != 200 or not isinstance(body, list):
            print(
                f"node{node.index}: could not load skill names ({status})",
                file=sys.stderr,
            )
            continue
        for skill in body:
            digest = str(skill.get("digest", "")).removeprefix("0x").lower()
            name = skill.get("name")
            if digest and isinstance(name, str):
                names[digest] = name
    return names


def enrich_with_skill_names(body: Any, names: dict[str, str]) -> Any:
    if not isinstance(body, dict):
        return body

    for result in body.get("results", []):
        for match in result.get("matches", []):
            digest = str(match.get("skill_digest", "")).removeprefix("0x").lower()
            match["skill_name"] = names.get(digest, "unknown")

    for match in body.get("aggregated", []):
        digest = str(match.get("skill_digest", "")).removeprefix("0x").lower()
        match["skill_name"] = names.get(digest, "unknown")

    return body


def require_query_id(body: Any) -> str:
    if not isinstance(body, dict):
        raise RuntimeError(  # noqa: TRY004 - malformed response, not a caller type error
            f"search/new returned non-object body: {body!r}"
        )
    query_id = body.get("query_id")
    if not isinstance(query_id, str) or not query_id:
        raise RuntimeError(f"search/new did not return a query_id: {body!r}")
    normalized = query_id.removeprefix("0x")
    if len(normalized) != 64:
        raise RuntimeError(f"query_id is not a 32-byte hex string: {query_id!r}")
    return query_id


def run_lifecycle(
    *,
    nodes_json: Path = DEFAULT_NODES_JSON,
    query: str,
    k: int = 5,
    initiator_index: int = 1,
    timeout: float = 120.0,
    poll_interval: float = 1.0,
    verbose: bool = False,
) -> LifecycleResult:
    """Run one search from creation to result retrieval.

    Raises `RuntimeError` on any failure so the CLI and the integration test can
    distinguish a failed lifecycle from a successful one.
    """

    lyquid_id, nodes = load_nodes(nodes_json)
    by_index = {node.index: node for node in nodes}
    if initiator_index not in by_index:
        raise RuntimeError(f"initiator must be one of {sorted(by_index)}")
    initiator = by_index[initiator_index]

    print(f"Lyquid: {lyquid_id}")
    print(f"Initiator: node{initiator.index} ({initiator.node_id})")

    with httpx.Client() as client:
        status, body = post_json(
            client,
            initiator,
            "/search/new",
            {"query": query, "k": k},
            timeout=timeout,
        )
        print(f"POST /search/new -> {status}")
        if verbose:
            print(json.dumps(body, indent=2))
        if status != 202:
            raise RuntimeError(f"search creation failed: {status} {body!r}")

        query_id = require_query_id(body)
        print(f"query_id: {query_id}")

        # Wait until the certified query call has been executed on-chain.
        registration_deadline = time.monotonic() + timeout
        while True:
            status, registered = post_json(
                client,
                initiator,
                "/search/status",
                {"query_id": query_id},
                timeout=30.0,
            )
            if status == 200 and isinstance(registered, dict):
                print("query registered")
                if verbose:
                    print(json.dumps(registered, indent=2))
                break
            if time.monotonic() >= registration_deadline:
                raise RuntimeError(f"query was not registered within {timeout:.0f}s")
            time.sleep(poll_interval)

        deadline = time.monotonic() + timeout
        final_status: dict[str, Any] | None = None
        while True:
            status, status_body = post_json(
                client,
                initiator,
                "/search/status",
                {"query_id": query_id},
                timeout=30.0,
            )
            print(f"POST /search/status -> {status}")
            if verbose:
                print(json.dumps(status_body, indent=2))
            elif isinstance(status_body, dict):
                print(
                    f"  completed={status_body.get('completed')} "
                    f"received={status_body.get('received_results')} "
                    f"expected={status_body.get('expected_results')}"
                )

            if status != 200:
                raise RuntimeError(f"search status failed: {status} {status_body!r}")
            if isinstance(status_body, dict) and status_body.get("completed"):
                final_status = status_body
                break
            if time.monotonic() >= deadline:
                raise RuntimeError(f"search did not complete within {timeout:.0f}s")
            time.sleep(poll_interval)

        status, results = post_json(
            client,
            initiator,
            "/search/get",
            {"query_id": query_id},
            timeout=30.0,
        )
        names = load_skill_names(client, nodes)
        results = enrich_with_skill_names(results, names)
        print(f"POST /search/get -> {status}")
        if verbose:
            print(json.dumps(results, indent=2))
        if status != 200 or not isinstance(results, dict):
            raise RuntimeError(f"search/get failed: {status} {results!r}")

        if final_status is None:
            raise RuntimeError("search completed without a status body")
        return LifecycleResult(query_id=query_id, status=final_status, results=results)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--nodes-json", type=Path, default=DEFAULT_NODES_JSON)
    parser.add_argument("--query", required=True, help="search keywords")
    parser.add_argument(
        "-v", "--verbose", action="store_true", help="print response bodies"
    )
    parser.add_argument("--k", type=int, default=5, help="results per peer")
    parser.add_argument(
        "--initiator",
        type=int,
        default=1,
        help="1-based node index that creates the search (default: 1)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=120.0,
        help="seconds to wait for the search to complete",
    )
    parser.add_argument(
        "--poll-interval", type=float, default=1.0, help="status polling interval"
    )
    args = parser.parse_args()

    try:
        result = run_lifecycle(
            nodes_json=args.nodes_json,
            query=args.query,
            k=args.k,
            initiator_index=args.initiator,
            timeout=args.timeout,
            poll_interval=args.poll_interval,
            verbose=args.verbose,
        )
    except RuntimeError as error:
        print(str(error), file=sys.stderr)
        return 1

    print("\nAggregated results:")
    for match in result.results.get("aggregated", []):
        print(f"  {match.get('skill_name', 'unknown')}: {match.get('bm25_score')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
