from collections.abc import Mapping
from dataclasses import asdict, dataclass
from typing import Any, Self

import httpx

from .skills import Skill
from .utils import CLIArgs


@dataclass(frozen=True)
class SkillSummary:
    """A skill entry returned by ``GET /skills/list``."""

    name: str
    description: str
    digest: str

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Self:
        return cls(
            name=data["name"],
            description=data["description"],
            digest=data["digest"],
        )


@dataclass(frozen=True)
class UploadSkillResponse:
    """The response returned by ``POST /skills/upload``."""

    uploaded: int
    digests: list[str]

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Self:
        return cls(
            uploaded=data["uploaded"],
            digests=list(data.get("digests", [])),
        )


def _strip_id_prefix(value: str, prefix: str) -> str:
    return value.strip().removeprefix(prefix)


def _virtual_host(lyquid_id: str | None, node_id: str | None) -> str | None:
    if not lyquid_id or not node_id:
        return None
    lyquid_label = _strip_id_prefix(lyquid_id, "Lyquid-")
    node_label = _strip_id_prefix(node_id, "Node-")
    return f"{lyquid_label}.{node_label}"


def _skill_payload(skill: Skill | Mapping[str, Any] | Any) -> dict[str, Any]:
    if isinstance(skill, Mapping):
        return dict(skill)
    if hasattr(skill, "to_dict"):
        return skill.to_dict()
    return asdict(skill)


class NodeManager:
    """HTTP client for the Mead Lyquid's instance endpoints.

    Requests are routed to the Lyquid by the virtual ``Host`` header built from
    the Lyquid and node IDs, as described in the HTTP endpoint documentation.
    """

    def __init__(self, cli_args: CLIArgs):
        self._server_url = cli_args.server_url.rstrip("/")
        self._host = _virtual_host(cli_args.lyquid_id, cli_args.node_id)

    def _headers(self, *, content_type: str | None = None) -> dict[str, str]:
        headers = {"accept": "application/json"}
        if content_type is not None:
            headers["content-type"] = content_type
        if self._host is not None:
            headers["Host"] = self._host
        return headers

    def _url(self, path: str) -> str:
        return f"{self._server_url}/{path.lstrip('/')}"

    def list_skills(self) -> list[SkillSummary]:
        """Return the skills registered on the target node."""

        response = httpx.get(
            self._url("/skills/list"),
            headers=self._headers(),
        )
        if response.is_error:
            response.raise_for_status()
        return [SkillSummary.from_dict(entry) for entry in response.json()]

    def upload_skill(self, skill: Skill) -> UploadSkillResponse:
        """Upload a single skill to the target node."""

        response = httpx.post(
            self._url("/skills/upload"),
            headers=self._headers(content_type="application/json"),
            json=_skill_payload(skill),
        )
        if response.is_error:
            response.raise_for_status()
        return UploadSkillResponse.from_dict(response.json())


__all__ = [
    "NodeManager",
    "SkillSummary",
    "UploadSkillResponse",
]
