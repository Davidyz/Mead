"""MCP tools backed by the Mead Lyquid HTTP API."""

import mimetypes
from collections.abc import Iterable
from typing import Any
from urllib.parse import quote

from mcp.types import EmbeddedResource, TextContent, TextResourceContents
lazy from mcp.server import MCPServer

from .node_manager import NodeManager, SkillSummary
from .skills import Skill, SkillResource

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
    paths: Iterable[str] | None,
) -> list[SkillContent]:
    resources: dict[str, SkillResource] = {
        resource.path: resource for resource in skill.resources
    }
    resources["SKILL.md"] = SkillResource(
        path="SKILL.md", contents=skill.instructions
    )
    selected = list(dict.fromkeys(paths)) if paths is not None else ["SKILL.md"]

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
    actual_resources = {resource.path: resource for resource in skill.resources}
    if actual_resources:
        summary.append("Available resources:")
        summary.extend(f"- {path}" for path in sorted(actual_resources))

    blocks: list[SkillContent] = [
        TextContent(type="text", text="\n".join(summary)),
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
        paths: Iterable[str] | None = None,
    ) -> list[SkillContent]:
        """Return a skill by digest.

        Returns a text summary block plus embedded resource files. The
        ``paths`` parameter defaults to ``["SKILL.md"]``, so calling with just
        ``digest`` fetches the main skill body. The summary lists all available
        resource paths.

        The first call for any skill should always be with just ``digest`` to
        fetch ``SKILL.md``. Inspect the summary, then call again with
        ``paths=[...]`` to fetch specific files. You can omit ``SKILL.md`` from
        ``paths`` on subsequent calls since the agent already has it from the
        initial fetch.

        Any file assets referenced by the skill or by other tools in the Mead
        MCP must be fetched via this tool using ``paths``, not via external
        file I/O. The returned resources come with URIs of the form
        ``skill://<digest>/<path>`` for use with MCP embedded resources.
        """
        return _render_skill(
            digest,
            self._node_manager.get_skill(digest),
            paths,
        )

    def make_mcp_server(self) -> MCPServer:
        mcp_server = MCPServer("mead")
        mcp_server.add_tool(self.mcp_search_skills, name="search_skills")
        mcp_server.add_tool(self.mcp_get_skill, name="get_skill")
        return mcp_server


__all__ = ["MeadTools"]
