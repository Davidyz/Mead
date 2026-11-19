import asyncio
import logging
import sys
from pathlib import Path

import httpx
from dotenv import load_dotenv

from .node_manager import NodeManager
from .skills import SkillManager
from .tools import MeadTools
from .utils import find_env, parse_args

logger = logging.getLogger(name=__name__)

# Resolve .env files relative to this package's directory so the lookup
# works regardless of the caller's working directory (IDEs often launch
# MCP servers from arbitrary directories).
_SCRIPT_DIR = Path(__file__).resolve().parent


def _find_env_in_tree(filename: str) -> Path | None:
    """Look for *filename* relative to the script directory, then walk up."""
    candidate = _SCRIPT_DIR / filename
    if candidate.is_file():
        return candidate
    return find_env(filename)


async def main_coro() -> None:
    # Prefer .env.mead.dev (written by devnet bootstrap) so that a user's
    # personal .env is never polluted by test artefacts.  Resolve relative
    # to this package's directory first, then fall back to a cwd-based
    # walk-up so IDEs launching from arbitrary directories still work.
    dev_env = _find_env_in_tree(".env.mead.dev")
    if dev_env is not None:
        load_dotenv(dev_env)
    else:
        workspace_env = find_env(".env")
        if workspace_env is not None:
            load_dotenv(workspace_env)
    cli_args = parse_args()

    node_manager = NodeManager(cli_args)

    uploaded_skill_count = 0
    loaded_skills = []
    if cli_args.skill_dir is not None:
        skill_manager = SkillManager(cli_args.skill_dir)
        skill_manager.load_skills()
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
        if cli_args.skill_dir is None:
            print("No skill directory provided; skipping skill upload.")
        else:
            print(f"Loaded {len(loaded_skills)} skills.")
        if uploaded_skill_count != len(loaded_skills):
            print(f"Only {uploaded_skill_count} skills were uploaded successfully.")
            sys.exit(1)
        try:
            result = node_manager.prepare(
                activate_oracle=cli_args.activate_oracle,
                timeout=cli_args.wait_timeout,
                poll_interval=cli_args.poll_interval,
            )
        except (httpx.HTTPError, RuntimeError, TypeError, ValueError) as error:
            print(f"Failed to prepare node: {error}", file=sys.stderr)
            sys.exit(1)
        print(
            f"Node {result.node_id} is ready: "
            f"oracle epoch {result.oracle.epoch}, "
            f"{len(result.nodes)} registered node(s)."
        )
        sys.exit(0)

    server = MeadTools(node_manager).make_mcp_server()
    await server.run_stdio_async()


def main():
    asyncio.run(main_coro())
