"""Deploy Mead and prepare per-node skill fixtures for integration tests.

This performs the full local devnet preparation:

1. Discover the four node containers with ``docker container ls`` and read each
   node's ID from its container logs.
2. Publish and deploy the repository's Lyquid with
   ``shaker deploy Cargo.toml --reference <ref> -i <constructor> -o json``,
   unless an existing Lyquid ID is supplied with ``--lyquid-id``. The
   constructor input ABI-encodes the node IDs as the search oracle committee and
   the configured threshold. Publishing to the devnet's OCI registry lets the
   other nodes pull the image; without it only the node that received the deploy
   can host the Lyquid.
3. Create ``tests/fixture/node{1,2,3,4}/`` and copy four supported skills into
   each node directory.
4. Write ``tests/fixture/nodes.json`` with the Lyquid ID, node IDs, containers,
   ports, and selected fixture skill names for integration tests.
5. Upload each node's fixture directory with ``uv run mead --upload-only``.
6. Activate the constructor-staged search oracle through ``POST /oracle/activate``
   and wait for ``GET /oracle/status`` to report it active.
7. Self-register every node through ``POST /nodes/register`` and wait for the
   shared ``NodeRegistry`` to contain all of them.

Running this script is the full devnet bootstrap; no separate setup-search step
is required.

Skills containing non-UTF-8 resources are excluded because the canonical skill
format only supports text resources.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import random
import re
import shutil
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import Any

import httpx
from rich import print

from mead.node_manager import NodeManager, SkillSummary
from mead.skills import Skill
from mead.utils import CLIArgs

ROOT = Path(__file__).resolve().parent.parent
SKILLS_DIR = ROOT / "tests" / "skills" / "skills"
FIXTURE_DIR = ROOT / "tests" / "fixture"

NODE_COUNT = 4
SKILLS_PER_NODE = 4
DEFAULT_NODE_PORT = 10087
NODE_PORT_STEP = 1000
NODE_READY_TIMEOUT = 60.0

# OCI reference used to publish the built Lyquid. The multi-node Docker devnet
# exposes its registry on port 8000, and nodes fall back to
# ``http://registry:8000/lyquids`` (see the devnet's ``nodeN.toml``). The tag is
# arbitrary because deployments are pinned to the pushed digest.
DEFAULT_LYQUID_REFERENCE = "http://127.0.0.1:8000/lyquids:mead"

NODE_CONTAINER_RE = re.compile(r"(?:^|[-_])node(?P<index>\d+)(?:[-_]|$)")
NODE_ID_RE = re.compile(
    r"lyquor_tls::generator: Node ID:\s*(?:Node-)?(?P<label>[a-z0-9]+)"
)
NODE_PORT_RE = re.compile(r":(?P<port>\d+)->10087/tcp")

DEFAULT_ORACLE_THRESHOLD = 2
NODE_ID_BYTES = 32
DEFAULT_DEVNET_DIR = Path.home() / ".shakenup" / "ldk" / "docker" / "multi"
DEFAULT_NODE_FUNDING_ETH = "10"
ORACLE_ACTIVATION_TIMEOUT = 120.0
NODE_REGISTRATION_TIMEOUT = 60.0


@dataclass(frozen=True)
class Node:
    index: int
    container: str
    node_id: str
    port: int


def node_id_public_key(node_id: str) -> bytes:
    """Decode a `Node-...` identifier to its 32-byte Ed25519 public key."""
    label = node_id.removeprefix("Node-")
    padding = "=" * ((8 - len(label) % 8) % 8)
    try:
        decoded = base64.b32decode(label.upper() + padding, casefold=True)
    except Exception as error:
        raise RuntimeError(f"invalid node ID {node_id!r}: {error}") from error
    if len(decoded) < NODE_ID_BYTES:
        raise RuntimeError(f"invalid node ID {node_id!r}: too short")
    return decoded[:NODE_ID_BYTES]


def encode_constructor_input(committee: list[str], threshold: int) -> str:
    """ABI-encode `(bytes32[] committee, uint16 threshold)` for `shaker deploy -i`."""
    if not committee:
        raise ValueError("oracle committee must not be empty")
    if not 0 < threshold <= len(committee):
        raise ValueError(
            f"threshold {threshold} must be between 1 and {len(committee)}"
        )

    keys = [node_id_public_key(node_id) for node_id in committee]
    # Parameter head: offset to the dynamic array, then the uint16 threshold.
    head = (64).to_bytes(32, "big") + threshold.to_bytes(32, "big")
    # Dynamic array tail: length followed by one 32-byte word per element.
    tail = len(keys).to_bytes(32, "big") + b"".join(keys)
    return "0x" + (head + tail).hex()


def run(
    command: list[str], *, capture_output: bool = False
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            command,
            cwd=ROOT,
            check=True,
            text=True,
            capture_output=capture_output,
        )
    except subprocess.CalledProcessError as error:
        print(
            f"Command failed ({error.returncode}): {' '.join(command)}",
            file=sys.stderr,
        )
        if error.stdout:
            print(error.stdout, file=sys.stderr, end="")
        if error.stderr:
            print(error.stderr, file=sys.stderr, end="")
        raise


def parse_json_output(output: str) -> dict[str, object]:
    for line in reversed(output.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            decoded = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(decoded, dict):
            return decoded
    raise RuntimeError("command did not produce a JSON object")


def deploy_lyquid(nodes: list[Node], threshold: int) -> str:
    reference = os.environ.get("LYQUID_REFERENCE", DEFAULT_LYQUID_REFERENCE)
    committee = [node.node_id for node in sorted(nodes, key=lambda node: node.index)]
    constructor_input = encode_constructor_input(committee, threshold)
    print(f"Deploying Cargo.toml to {reference}...")
    print(f"Oracle committee ({threshold} of {len(committee)}):")
    for node in sorted(nodes, key=lambda node: node.index):
        print(f"  - {node.node_id}")
    print(f"Constructor input: {constructor_input}")
    process = run(
        [
            "shaker",
            "deploy",
            "Cargo.toml",
            "--reference",
            reference,
            "-i",
            constructor_input,
            "-o",
            "json",
        ],
        capture_output=True,
    )
    if process.stderr:
        print(process.stderr, file=sys.stderr, end="")

    deployment = parse_json_output(process.stdout)
    lyquid_id = deployment.get("lyquid_id")
    if isinstance(lyquid_id, str) and lyquid_id.startswith("Lyquid-"):
        return lyquid_id

    contract = deployment.get("contract")
    if not isinstance(contract, str):
        raise TypeError("shaker deploy output did not contain a contract or Lyquid ID")

    # Deployment may return before the node has resolved the new Lyquid ID.
    for _ in range(15):
        listed = parse_json_output(
            run(["shaker", "list", "-o", "json"], capture_output=True).stdout
        )
        lyquids = listed.get("lyquids", [])
        if isinstance(lyquids, list):
            for entry in lyquids:
                if (
                    isinstance(entry, dict)
                    and str(entry.get("contract", "")).lower() == contract.lower()
                    and isinstance(entry.get("lyquid_id"), str)
                ):
                    return entry["lyquid_id"]
        time.sleep(1)

    raise RuntimeError(
        f"could not resolve a Lyquid ID for deployed contract {contract}"
    )


def discover_node_containers() -> dict[int, tuple[str, int]]:
    process = run(
        ["docker", "container", "ls", "--format", "{{.Names}}\t{{.Ports}}"],
        capture_output=True,
    )
    nodes: dict[int, tuple[str, int]] = {}
    for line in process.stdout.splitlines():
        name, _, ports = line.partition("\t")
        match = NODE_CONTAINER_RE.search(name)
        if match is None:
            continue
        index = int(match.group("index"))
        port_match = NODE_PORT_RE.search(ports)
        port = (
            int(port_match.group("port"))
            if port_match is not None
            else DEFAULT_NODE_PORT + NODE_PORT_STEP * (index - 1)
        )
        nodes[index] = (name, port)

    missing = sorted(set(range(1, NODE_COUNT + 1)) - nodes.keys())
    if missing:
        names = [name for name, _ in nodes.values()]
        raise RuntimeError(
            f"could not find containers for node(s) {missing}; saw: {names}"
        )
    return nodes


def node_id_from_logs(container: str) -> str:
    process = run(["docker", "logs", container], capture_output=True)
    logs = process.stdout + process.stderr
    matches = list(NODE_ID_RE.finditer(logs))
    if not matches:
        raise RuntimeError(f"could not find a node ID in logs for {container}")
    return f"Node-{matches[-1].group('label')}"


def node_from_container(index: int, container: str, port: int) -> Node:
    node_id = node_id_from_logs(container)
    print(f"node{index}: {node_id} ({container}, port {port})")
    return Node(index=index, container=container, node_id=node_id, port=port)


def find_anvil_container(explicit: str | None = None) -> str:
    if explicit is not None:
        return explicit
    names = run(
        ["docker", "container", "ls", "--format", "{{.Names}}"],
        capture_output=True,
    ).stdout.splitlines()
    for name in names:
        if "anvil" in name:
            return name
    raise RuntimeError("could not find the devnet anvil container")


def node_submitter_address(anvil_container: str, private_key: str) -> str:
    return run(
        [
            "docker",
            "exec",
            anvil_container,
            "cast",
            "wallet",
            "address",
            "--private-key",
            private_key,
        ],
        capture_output=True,
    ).stdout.strip()


def cast_balance(anvil_container: str, address: str) -> int:
    output = run(
        [
            "docker",
            "exec",
            anvil_container,
            "cast",
            "balance",
            "--rpc-url",
            "http://127.0.0.1:8545",
            address,
        ],
        capture_output=True,
    ).stdout.strip()
    return int(output.split()[0])


def fund_nodes(
    nodes: list[Node],
    devnet_dir: Path,
    *,
    anvil_container: str | None,
    funding_eth: str,
) -> None:
    """Top up each node submitter from the first devnet submitter account."""
    if not devnet_dir.is_dir():
        print(f"Skipping node funding: {devnet_dir} does not exist")
        return

    configs: dict[int, dict[str, Any]] = {}
    for node in nodes:
        path = devnet_dir / f"node{node.index}.toml"
        if not path.is_file():
            print(f"Skipping node funding: {path} does not exist")
            return
        with path.open("rb") as file:
            configs[node.index] = tomllib.load(file)

    try:
        funder_config = configs[1]["submitter"]["inline"]
    except (KeyError, TypeError) as error:
        raise RuntimeError(
            f"could not read the node1 submitter key: {error}"
        ) from error
    if not isinstance(funder_config, str):
        raise TypeError("node1 submitter.inline must be a string private key")

    anvil = find_anvil_container(anvil_container)
    funder = node_submitter_address(anvil, funder_config)
    target_balance = int(Decimal(str(funding_eth)) * Decimal(10**18))
    print(f"Funding node submitters from {funder} with {funding_eth} ETH minimum...")

    for node in nodes:
        config = configs[node.index]
        try:
            private_key = config["submitter"]["inline"]
        except KeyError, TypeError:
            print(f"node{node.index}: no submitter.inline private key; skipping")
            continue
        if not isinstance(private_key, str):
            print(f"node{node.index}: invalid submitter.inline; skipping")
            continue
        address = node_submitter_address(anvil, private_key)
        balance = cast_balance(anvil, address)
        if balance >= target_balance:
            print(f"node{node.index}: {address} already has {balance} wei")
            continue
        run(
            [
                "docker",
                "exec",
                anvil,
                "cast",
                "send",
                "--rpc-url",
                "http://127.0.0.1:8545",
                "--private-key",
                funder_config,
                "--value",
                f"{funding_eth}ether",
                address,
            ],
            capture_output=True,
        )
        print(f"node{node.index}: funded {address} with {funding_eth} ETH")


def has_only_text_resources(skill_dir: Path) -> bool:
    for path in skill_dir.rglob("*"):
        relative = path.relative_to(skill_dir)
        if any(part.startswith(".") for part in relative.parts):
            continue
        if not path.is_file() or path.name.lower() == "skill.md":
            continue
        try:
            path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            return False
    return True


def supported_skills() -> list[Path]:
    if not SKILLS_DIR.is_dir():
        raise RuntimeError(
            f"{SKILLS_DIR} is missing; run `make prepare-skills` first to fetch the skill catalog"
        )

    supported: list[Path] = []
    skipped: list[Path] = []
    for manifest in sorted(SKILLS_DIR.rglob("SKILL.md")):
        skill_dir = manifest.parent
        relative = skill_dir.relative_to(SKILLS_DIR)
        if any(part.startswith(".") for part in relative.parts):
            continue
        if has_only_text_resources(skill_dir):
            supported.append(skill_dir)
        else:
            skipped.append(skill_dir)

    if skipped:
        print(f"Excluding {len(skipped)} skill(s) with non-text resources.")
    if len(supported) < NODE_COUNT * SKILLS_PER_NODE:
        raise RuntimeError(
            f"need at least {NODE_COUNT * SKILLS_PER_NODE} supported skills, "
            f"found {len(supported)}"
        )
    return supported


def select_fixture_skills(skills: list[Path]) -> list[list[Path]]:
    seed = os.environ.get("MEAD_FIXTURE_SEED")
    rng = random.Random(int(seed)) if seed is not None else random.Random()
    selected = rng.sample(skills, NODE_COUNT * SKILLS_PER_NODE)
    return [
        selected[index * SKILLS_PER_NODE : (index + 1) * SKILLS_PER_NODE]
        for index in range(NODE_COUNT)
    ]


def prepare_fixture(assignments: list[list[Path]]) -> dict[int, Path]:
    if FIXTURE_DIR.exists():
        shutil.rmtree(FIXTURE_DIR)

    fixture_dirs: dict[int, Path] = {}
    for index, skill_dirs in enumerate(assignments, start=1):
        node_dir = FIXTURE_DIR / f"node{index}"
        node_dir.mkdir(parents=True)
        for skill_dir in skill_dirs:
            shutil.copytree(skill_dir, node_dir / skill_dir.name)
        fixture_dirs[index] = node_dir
        names = ", ".join(skill_dir.name for skill_dir in skill_dirs)
        print(f"node{index}: {names}")
    return fixture_dirs


def write_nodes_json(
    nodes: list[Node],
    lyquid_id: str,
    assignments: list[list[Path]],
) -> Path:
    path = FIXTURE_DIR / "nodes.json"
    payload = {
        "lyquid_id": lyquid_id,
        "nodes": [
            {
                "index": node.index,
                "node_id": node.node_id,
                "container": node.container,
                "port": node.port,
                "skills": [
                    Skill(skill_dir).name for skill_dir in assignments[node.index - 1]
                ],
            }
            for node in nodes
        ],
    }
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {path.relative_to(ROOT)}")
    return path


def upload_skills(node: Node, fixture_dir: Path, lyquid_id: str) -> None:
    wait_for_lyquid(node, lyquid_id)
    print(f"Uploading {fixture_dir.relative_to(ROOT)} to node{node.index}...")
    run(
        [
            "uv",
            "run",
            "mead",
            "--upload-only",
            "--server-url",
            f"http://127.0.0.1:{node.port}",
            "--skill-dir",
            str(fixture_dir),
            "--node-id",
            node.node_id,
            "--lyquid-id",
            lyquid_id,
        ]
    )


def _node_headers(node: Node, lyquid_id: str) -> dict[str, str]:
    lyquid_label = lyquid_id.removeprefix("Lyquid-")
    node_label = node.node_id.removeprefix("Node-")
    return {
        "accept": "application/json",
        "Host": f"{lyquid_label}.{node_label}",
    }


def wait_for_lyquid(node: Node, lyquid_id: str) -> None:
    headers = _node_headers(node, lyquid_id)
    url = f"http://127.0.0.1:{node.port}/skills/list"
    deadline = time.monotonic() + NODE_READY_TIMEOUT

    print(f"Waiting for {lyquid_id} on node{node.index}...")
    last_error = "no response"
    while time.monotonic() < deadline:
        try:
            response = httpx.get(url, headers=headers, timeout=5.0)
        except httpx.HTTPError as error:
            last_error = str(error)
        else:
            if response.status_code == 200:
                return
            last_error = f"HTTP {response.status_code}: {response.text[:200]}"
        time.sleep(1)

    raise RuntimeError(
        f"Lyquid {lyquid_id} did not become ready on node{node.index}: {last_error}"
    )


def activate_search_oracle(node: Node, lyquid_id: str) -> None:
    """Activate the first search-oracle epoch and wait until it is active.

    POST once to submit the needed epoch work, then poll GET /oracle/status
    (read-only). Re-POST with a capped exponential backoff only while the
    oracle is still inactive, so polling does not re-submit a finalize
    certificate every second.
    """
    headers = _node_headers(node, lyquid_id)
    base_url = f"http://127.0.0.1:{node.port}"
    activate_url = f"{base_url}/oracle/activate"
    status_url = f"{base_url}/oracle/status"

    print(f"Activating search oracle via node{node.index}...")
    deadline = time.monotonic() + ORACLE_ACTIVATION_TIMEOUT
    next_post = time.monotonic()
    backoff = 1.0
    last_error = "no response"

    while time.monotonic() < deadline:
        now = time.monotonic()
        if now >= next_post:
            try:
                response = httpx.post(activate_url, headers=headers, timeout=30.0)
            except httpx.HTTPError as error:
                last_error = str(error)
            else:
                if response.status_code == 200:
                    body = response.json()
                    if body.get("status") == "active":
                        print(f"Search oracle active (epoch {body.get('epoch')}).")
                        return
                last_error = f"HTTP {response.status_code}: {response.text[:200]}"
            next_post = now + backoff
            backoff = min(backoff * 2, 8.0)

        try:
            status = httpx.get(status_url, headers=headers, timeout=5.0)
        except httpx.HTTPError as error:
            last_error = str(error)
        else:
            if status.status_code == 200:
                body = status.json()
                if body.get("active"):
                    print(f"Search oracle active (epoch {body.get('epoch')}).")
                    return
                last_error = (
                    f"epoch {body.get('epoch')} source_epoch {body.get('source_epoch')}"
                )
            else:
                last_error = f"HTTP {status.status_code}: {status.text[:200]}"
        time.sleep(1)

    raise RuntimeError(
        "search oracle did not become active within "
        f"{ORACLE_ACTIVATION_TIMEOUT:.0f}s: {last_error}"
    )


def register_node(node: Node, lyquid_id: str) -> None:
    """Submit this node's certified self-registration.

    The endpoint returns 200 when the node is already in the registry and 202
    when a certified registration call was just submitted. Retry while the
    oracle is still becoming active (503).
    """
    headers = _node_headers(node, lyquid_id)
    url = f"http://127.0.0.1:{node.port}/nodes/register"

    deadline = time.monotonic() + NODE_REGISTRATION_TIMEOUT
    last_error = "no response"
    while time.monotonic() < deadline:
        try:
            response = httpx.post(url, headers=headers, timeout=60.0)
        except httpx.HTTPError as error:
            last_error = str(error)
        else:
            if response.status_code in (200, 202):
                print(
                    f"node{node.index} ({node.node_id}): "
                    f"registered (HTTP {response.status_code})"
                )
                return
            last_error = f"HTTP {response.status_code}: {response.text[:200]}"
        time.sleep(1)

    raise RuntimeError(
        f"node{node.index} ({node.node_id}) failed to register within "
        f"{NODE_REGISTRATION_TIMEOUT:.0f}s: {last_error}"
    )


def wait_for_node_registry(nodes: list[Node], lyquid_id: str) -> None:
    """Wait until the shared NodeRegistry contains every prepared node."""
    probe = nodes[0]
    headers = _node_headers(probe, lyquid_id)
    url = f"http://127.0.0.1:{probe.port}/nodes/list"
    expected = sorted(node.node_id for node in nodes)

    deadline = time.monotonic() + NODE_REGISTRATION_TIMEOUT
    last_error = "no response"
    while time.monotonic() < deadline:
        try:
            response = httpx.get(url, headers=headers, timeout=5.0)
        except httpx.HTTPError as error:
            last_error = str(error)
        else:
            if response.status_code == 200:
                try:
                    actual = sorted(response.json().get("nodes", []))
                except ValueError:
                    last_error = "invalid /nodes/list response"
                else:
                    if actual == expected:
                        print(
                            f"NodeRegistry contains all {len(expected)} "
                            "registered nodes."
                        )
                        return
                    last_error = f"registered {len(actual)}/{len(expected)}"
            else:
                last_error = f"HTTP {response.status_code}: {response.text[:200]}"
        time.sleep(1)

    raise RuntimeError(
        "NodeRegistry did not contain all nodes within "
        f"{NODE_REGISTRATION_TIMEOUT:.0f}s: {last_error}"
    )


def register_nodes(nodes: list[Node], lyquid_id: str) -> None:
    for node in nodes:
        register_node(node, lyquid_id)
    wait_for_node_registry(nodes, lyquid_id)


def list_deployed_skills(node: Node, lyquid_id: str) -> list[SkillSummary]:
    node_manager = NodeManager(
        CLIArgs(
            server_url=f"http://127.0.0.1:{node.port}",
            skill_dir=SKILLS_DIR,
            node_id=node.node_id,
            lyquid_id=lyquid_id,
        )
    )
    return node_manager.list_skills()


def collect_deployed_skills(
    nodes: list[Node], lyquid_id: str
) -> dict[int, list[SkillSummary]]:
    return {node.index: list_deployed_skills(node, lyquid_id) for node in nodes}


def print_deployed_skills(
    nodes: list[Node], deployed_skills: dict[int, list[SkillSummary]]
) -> None:
    print("\nDeployed skills by node:")
    for node in nodes:
        print(f"node{node.index} ('{node.node_id}'):")
        for skill in deployed_skills[node.index]:
            print(f"  - {skill.name}")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Deploy Mead and prepare per-node skill fixtures."
    )
    parser.add_argument(
        "--lyquid-id",
        help="Use an existing Lyquid ID instead of running `shaker deploy`.",
    )
    parser.add_argument(
        "--threshold",
        type=int,
        default=DEFAULT_ORACLE_THRESHOLD,
        help=(
            "Search oracle threshold passed to the constructor during deployment "
            f"(default: {DEFAULT_ORACLE_THRESHOLD})."
        ),
    )
    parser.add_argument(
        "--devnet-dir",
        type=Path,
        default=Path(os.environ.get("LYQUOR_DEVNET_DIR", DEFAULT_DEVNET_DIR)),
        help=(
            "Directory containing node1.toml..nodeN.toml for devnet funding "
            f"(default: {DEFAULT_DEVNET_DIR})."
        ),
    )
    parser.add_argument(
        "--funding-eth",
        default=os.environ.get("NODE_FUNDING_ETH", DEFAULT_NODE_FUNDING_ETH),
        help=(
            "Minimum ETH balance for each node submitter, funded from node1 "
            f"(default: {DEFAULT_NODE_FUNDING_ETH})."
        ),
    )
    parser.add_argument(
        "--anvil-container",
        help="Anvil container name (default: auto-detect).",
    )
    parser.add_argument(
        "--no-fund-nodes",
        action="store_true",
        help="Do not top up node submitter accounts.",
    )
    return parser.parse_args(argv)


def main() -> None:
    args = parse_args()

    containers = discover_node_containers()
    nodes = [
        node_from_container(index, *containers[index])
        for index in range(1, NODE_COUNT + 1)
    ]

    if not args.no_fund_nodes:
        fund_nodes(
            nodes,
            args.devnet_dir,
            anvil_container=args.anvil_container,
            funding_eth=str(args.funding_eth),
        )

    if args.lyquid_id:
        lyquid_id = args.lyquid_id
        print(f"Using Lyquid: '{lyquid_id}'")
    else:
        lyquid_id = deploy_lyquid(nodes, args.threshold)
        print(f"Deployed Lyquid: '{lyquid_id}'")

    skills = supported_skills()
    assignments = select_fixture_skills(skills)
    fixture_dirs = prepare_fixture(assignments)
    write_nodes_json(nodes, lyquid_id, assignments)

    for node in nodes:
        upload_skills(node, fixture_dirs[node.index], lyquid_id)

    activate_search_oracle(nodes[0], lyquid_id)
    register_nodes(nodes, lyquid_id)

    deployed_skills = collect_deployed_skills(nodes, lyquid_id)
    print_deployed_skills(nodes, deployed_skills)


if __name__ == "__main__":
    main()
