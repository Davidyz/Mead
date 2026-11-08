"""MCP tools backed by the Mead Lyquid HTTP API."""

from collections.abc import Iterable
from typing import Literal

lazy from mcp.server import MCPServer

from .node_manager import NodeManager, SkillSummary
from .skills import Skill


def _summary_payload(summary: SkillSummary) -> dict[str, str]:
    return {
        "name": summary.name,
        "description": summary.description,
        "digest": summary.digest,
    }


def _render_skill(
    skill: Skill,
    detail: Literal["description", "full"],
    paths: Iterable[str] | None,
) -> str:
    if detail == "description":
        return skill.description
    if detail != "full":
        raise ValueError(f"unsupported detail level: {detail}")

    resources = {resource.path: resource for resource in skill.resources}
    selected = sorted(resources) if paths is None else list(dict.fromkeys(paths))
    missing = [path for path in selected if path not in resources]
    if missing:
        raise ValueError(
            f"Skill {skill.name} does not contain resources: {', '.join(missing)}"
        )

    sections = [f"# {skill.name}", skill.description, skill.instructions]
    if resources:
        sections.append("## Available resources")
        sections.extend(f"- {path}" for path in sorted(resources))
    for path in selected:
        sections.append(f"## Resource: {path}\n\n{resources[path].contents}")
    return "\n\n".join(sections)


class MeadTools:
    """MCP tool implementations for listing, searching, and fetching skills."""

    def __init__(self, node_manager: NodeManager):
        self._node_manager = node_manager

    def mcp_list_skill(self) -> list[dict[str, str]]:
        """List every skill name, description, and digest on this node."""
        return [
            _summary_payload(summary)
            for summary in sorted(
                self._node_manager.list_skills(),
                key=lambda item: (item.name, item.digest),
            )
        ]

    def mcp_search_skills(self, query: str, top_k: int = 5) -> list[dict[str, str]]:
        """Search the network and return matching skill summaries."""
        return [
            _summary_payload(summary)
            for summary in self._node_manager.search_skills(query, top_k=top_k)
        ]

    def mcp_get_skill(
        self,
        digest: str,
        detail: Literal["description", "full"] = "full",
        paths: Iterable[str] | None = None,
    ) -> str:
        """Return a skill for an LLM agent, optionally including its files.

        ``detail="description"`` exposes only the description. ``detail="full"``
        exposes the instructions and the requested resource contents. Passing
        ``paths=None`` includes every resource; pass a subset to limit exposure.
        """
        return _render_skill(self._node_manager.get_skill(digest), detail, paths)

    def make_mcp_server(self) -> MCPServer:
        mcp_server = MCPServer("mead")
        mcp_server.add_tool(self.mcp_list_skill, name="list_skill")
        mcp_server.add_tool(self.mcp_search_skills, name="search_skills")
        mcp_server.add_tool(self.mcp_get_skill, name="get_skill")
        return mcp_server


__all__ = ["MeadTools"]
