import asyncio
import logging
import sys

import httpx

from .node_manager import NodeManager
from .skills import SkillManager
from .utils import parse_args

logger = logging.getLogger(name=__name__)


async def main_coro() -> None:
    cli_args = parse_args()

    node_manager = NodeManager(cli_args)
    skill_manager = SkillManager(cli_args.skill_dir)
    skill_manager.load_skills()

    uploaded_skill_count = 0
    loaded_skills = skill_manager.list_skills()
    for skill in loaded_skills:
        try:
            node_manager.upload_skill(skill)
            uploaded_skill_count += 1
        except httpx.HTTPError, KeyError, TypeError, ValueError:
            logger.exception("Failed to upload skill %s", skill.name)

    if cli_args.upload_only:
        print(f"Loaded {len(loaded_skills)} skills.")
        return_val = 0
        if uploaded_skill_count != len(loaded_skills):
            print(f"Only {uploaded_skill_count} skills were uploaded successfully.")
            return_val = 1
        print(
            f"Node {cli_args.node_id} now has {len(node_manager.list_skills())} skills."
        )
        sys.exit(return_val)

    if cli_args.prepare_only:
        if uploaded_skill_count != len(loaded_skills):
            print(f"Only {uploaded_skill_count} skills were uploaded successfully.")
            sys.exit(1)
        try:
            result = node_manager.prepare(
                activate_oracle=cli_args.activate_oracle,
                timeout=cli_args.wait_timeout,
                poll_interval=cli_args.poll_interval,
            )
        except (httpx.HTTPError, RuntimeError, ValueError) as error:
            print(f"Failed to prepare node: {error}", file=sys.stderr)
            sys.exit(1)
        print(
            f"Node {result.node_id} is ready: "
            f"oracle epoch {result.oracle.epoch}, "
            f"{len(result.nodes)} registered node(s)."
        )
        sys.exit(0)

    server = skill_manager.make_mcp_server()
    await server.run_stdio_async()


def main():
    asyncio.run(main_coro())
