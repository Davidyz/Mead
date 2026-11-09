"""MCP tools backed by the Mead Lyquid HTTP API."""

import mimetypes
from collections.abc import Iterable
from typing import Any, Literal
from urllib.parse import quote

from mcp.types import EmbeddedResource, TextContent, TextResourceContents
lazy from mcp.server import MCPServer

from .node_manager import NodeManager, SkillSummary
from .skills import Skill

type SkillDetail = Literal["skill_md", "full"]
type SkillContent = TextContent | EmbeddedResource


def _summary_payload(summary: SkillSummary) -> dict[str, str]:
    return {
        "name": summary.name,
        "description": summary.description,
        "digest": summary.digest,
    }


def _mime_type(path: str) -> str:
    return mimetypes.guess_type(path)[0] or "text/plain"


def _resource(
    digest: str, path: str, contents: str, mime_type: str
) -> EmbeddedResource:
    uri = f"skill://{digest}/{quote(path, safe='/')}"
    return EmbeddedResource(
        type="resource",
        resource=TextResourceContents(uri=uri, mime_type=mime_type, text=contents),
    )


def _render_skill(
    digest: str,
    skill: Skill,
    detail: SkillDetail,
    paths: Iterable[str] | None,
) -> list[SkillContent]:
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

    summary = [
        f"Skill: {skill.name}",
        f"Description: {skill.description}",
        f"Digest: {digest}",
    ]
    if resources:
        summary.append("Available resources:")
        summary.extend(f"- {path}" for path in sorted(resources))

    blocks: list[SkillContent] = [
        TextContent(type="text", text="\n".join(summary)),
        _resource(digest, "SKILL.md", skill.instructions, "text/markdown"),
    ]
    for path in selected:
        blocks.append(
            _resource(
                digest,
                path,
                resources[path].contents,
                _mime_type(path),
            )
        )
    return blocks


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

        Run searches one at a time. A search fans out to the network and can take
        a while to complete, because mead runs on a peer-to-peer network, so avoid
        issuing multiple ``search_skills`` calls concurrently.
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
    ) -> list[SkillContent]:
        """Return a skill by digest, starting with just its main body.

        Call this first with only ``digest`` (the default ``detail="skill_md"``).
        That returns a text summary block plus an embedded ``SKILL.md`` resource,
        without downloading any other resources. The summary lists the available
        resource paths.

        Inspect that list, then call again with ``paths=[...]`` to fetch only the
        resources you actually need. Do not use ``detail="full"`` unless you
        genuinely need every resource: full skill bodies can be very large.

        Each returned file is a separate MCP embedded resource with its own URI,
        so the skill's Markdown is not wrapped or concatenated with synthetic
        headings. ``detail="full"`` includes every resource; an explicit
        ``paths`` list includes those resources regardless of ``detail``.
        """
        return _render_skill(
            digest,
            self._node_manager.get_skill(digest),
            detail,
            paths,
        )

    def make_mcp_server(self) -> MCPServer:
        mcp_server = MCPServer("mead")
        mcp_server.add_tool(self.mcp_search_skills, name="search_skills")
        mcp_server.add_tool(self.mcp_get_skill, name="get_skill")
        return mcp_server


__all__ = ["MeadTools"]
