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

The constructor only stages the oracle committee. Activate the first epoch and
trigger node self-registration with ``scripts/setup_search.sh`` after deployment.

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
from dataclasses import dataclass
from pathlib import Path

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


def wait_for_lyquid(node: Node, lyquid_id: str) -> None:
    lyquid_label = lyquid_id.removeprefix("Lyquid-")
    node_label = node.node_id.removeprefix("Node-")
    headers = {
        "accept": "application/json",
        "Host": f"{lyquid_label}.{node_label}",
    }
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
    return parser.parse_args(argv)


def main() -> None:
    args = parse_args()

    containers = discover_node_containers()
    nodes = [
        node_from_container(index, *containers[index])
        for index in range(1, NODE_COUNT + 1)
    ]

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

    deployed_skills = collect_deployed_skills(nodes, lyquid_id)
    print_deployed_skills(nodes, deployed_skills)


if __name__ == "__main__":
    main()
