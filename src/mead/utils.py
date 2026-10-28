from argparse import ArgumentParser
from dataclasses import dataclass
from pathlib import Path


@dataclass
class CLIArgs:
    server_url: str
    skill_dir: Path
    upload_only: bool = False


def arg_parser() -> ArgumentParser:
    parser = ArgumentParser("mead")
    parser.add_argument("--server-url", help="URL to the lyquid node.")
    parser.add_argument(
        "--skill-dir", help="Path to the directory that contains skills."
    )
    parser.add_argument(
        "--upload-only",
        action="store_true",
        default=False,
        help="Only upload the skills to the node. Do not start the MCP.",
    )
    return parser


def parse_args(args: list[str] | None = None) -> CLIArgs:
    parsed = arg_parser().parse_args(args)
    return CLIArgs(
        server_url=parsed.server_url,
        skill_dir=parsed.skill_dir,
        upload_only=parsed.upload_only,
    )
