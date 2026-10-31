import asyncio
import sys

from .skills import SkillManager
from .utils import parse_args


async def main_coro() -> None:
    cli_args = parse_args()

    _skill_manager = SkillManager(cli_args.skill_dir)

    # TODO: Upload the skills

    if cli_args.upload_only:
        sys.exit(0)
    else:
        server = _skill_manager.make_mcp_server()
        await server.run_stdio_async()


def main():
    asyncio.run(main_coro())
