"""MCP tools backed by the Mead Lyquid HTTP API."""

from collections.abc import Iterable
from typing import Any, Literal

lazy from mcp.server import MCPServer

from .node_manager import NodeManager, SkillSummary
from .skills import Skill

type SkillDetail = Literal["skill_md", "full"]


def _summary_payload(summary: SkillSummary) -> dict[str, str]:
    return {
        "name": summary.name,
        "description": summary.description,
        "digest": summary.digest,
    }


def _render_skill(
    skill: Skill,
    detail: SkillDetail,
    paths: Iterable[str] | None,
) -> str:
    if detail not in ("skill_md", "full"):
        raise ValueError(f"unsupported detail level: {detail}")

    resources = {resource.path: resource for resource in skill.resources}
    if paths is not None:
        selected = list(dict.fromkeys(paths))
    elif detail == "full":
        selected = sorted(resources)
    else:
        selected = []

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
    """MCP tool implementations for searching and fetching skills."""

    def __init__(self, node_manager: NodeManager):
        self._node_manager = node_manager

    def mcp_search_skills(self, query: str, top_k: int = 5) -> dict[str, Any]:
        """Search the network for skills using a lexical BM25 query.

        Matching is exact-token based: the query and each skill are lowercased,
        split into words, and English stop words are removed. There is no
        stemming or synonym expansion, so include synonyms, related terms, and
        alternative word forms (for example, "image photo picture create
        generate draw") to improve recall.

        Returns ``skills`` (name, description, digest), ``timed_out``, the
        received/expected and verified/rejected result counts, and any
        ``unresolved_digests`` whose bodies were unavailable.

        These are summaries only. To read a skill, call ``get_skill`` with just
        its digest first, then request only the resources you actually need.
        """
        outcome = self._node_manager.search_skills(query, top_k=top_k)
        return {
            "skills": [_summary_payload(summary) for summary in outcome.skills],
            "timed_out": outcome.timed_out,
            "received_results": outcome.received_results,
            "expected_results": outcome.expected_results,
            "verified_results": outcome.verified_results,
            "rejected_results": outcome.rejected_results,
            "unresolved_digests": outcome.unresolved_digests,
        }

    def mcp_get_skill(
        self,
        digest: str,
        detail: SkillDetail = "skill_md",
        paths: Iterable[str] | None = None,
    ) -> str:
        """Return a skill by digest, starting with just its main body.

        Call this first with only ``digest`` (the default ``detail="skill_md"``).
        That returns the name, description, instructions, and the list of
        available resource paths without downloading any resources.

        Inspect that list, then call again with ``paths=[...]`` to fetch only the
        resources you actually need. Do not use ``detail="full"`` unless you
        genuinely need every resource: full skill bodies can be very large.

        ``detail="skill_md"`` returns the main body and lists available paths;
        ``detail="full"`` includes every resource. An explicit ``paths`` list
        includes those resources regardless of ``detail``.
        """
        return _render_skill(self._node_manager.get_skill(digest), detail, paths)

    def make_mcp_server(self) -> MCPServer:
        mcp_server = MCPServer("mead")
        mcp_server.add_tool(self.mcp_search_skills, name="search_skills")
        mcp_server.add_tool(self.mcp_get_skill, name="get_skill")
        return mcp_server


__all__ = ["MeadTools"]
