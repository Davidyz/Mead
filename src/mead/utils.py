import os
from argparse import ArgumentParser
from dataclasses import dataclass
from pathlib import Path


@dataclass
class CLIArgs:
    server_url: str
    skill_dir: Path
    node_id: str
    lyquid_id: str

    upload_only: bool = False
    prepare_only: bool = False
    activate_oracle: bool = False
    wait_timeout: float = 120.0
    poll_interval: float = 1.0


def arg_parser() -> ArgumentParser:
    parser = ArgumentParser("mead")
    parser.add_argument(
        "--server-url", help="URL to the lyquid node.", default="http://127.0.0.1:10087"
    )
    parser.add_argument(
        "--skill-dir",
        required=True,
        help="Path to the directory that contains skills.",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--upload-only",
        action="store_true",
        default=False,
        help="Only upload the skills to the node. Do not start the MCP.",
    )
    mode.add_argument(
        "--prepare-only",
        action="store_true",
        default=False,
        help=(
            "Upload skills, wait for the search oracle, self-register the node, "
            "and exit without starting the MCP."
        ),
    )
    parser.add_argument(
        "--activate-oracle",
        action="store_true",
        default=False,
        help=(
            "With --prepare-only, drive search-oracle activation. Only use this "
            "on a committee node."
        ),
    )
    parser.add_argument(
        "--wait-timeout",
        type=float,
        default=120.0,
        help="Seconds to wait for the oracle/registration in --prepare-only mode.",
    )
    parser.add_argument(
        "--poll-interval",
        type=float,
        default=1.0,
        help="Seconds between status/registry polls in --prepare-only mode.",
    )

    parser.add_argument("--node-id", help="ID of the lyquid node (default: $NODE_ID).")
    parser.add_argument("--lyquid-id", help="Lyquid ID to use (default: $LYQUID_ID).")
    return parser


def parse_args(args: list[str] | None = None) -> CLIArgs:
    parser = arg_parser()
    parsed = parser.parse_args(args)
    node_id = parsed.node_id or os.environ.get("NODE_ID")
    lyquid_id = parsed.lyquid_id or os.environ.get("LYQUID_ID")
    if not node_id:
        parser.error("--node-id or NODE_ID environment variable is required")
    if not lyquid_id:
        parser.error("--lyquid-id or LYQUID_ID environment variable is required")
    if parsed.activate_oracle and not parsed.prepare_only:
        parser.error("--activate-oracle requires --prepare-only")
    if parsed.wait_timeout <= 0:
        parser.error("--wait-timeout must be greater than zero")
    if parsed.poll_interval <= 0:
        parser.error("--poll-interval must be greater than zero")
    return CLIArgs(
        server_url=parsed.server_url,
        skill_dir=Path(parsed.skill_dir),
        upload_only=parsed.upload_only,
        prepare_only=parsed.prepare_only,
        activate_oracle=parsed.activate_oracle,
        wait_timeout=parsed.wait_timeout,
        poll_interval=parsed.poll_interval,
        lyquid_id=lyquid_id,
        node_id=node_id,
    )
