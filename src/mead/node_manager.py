import time
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
        data = _mapping(data)
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
        data = _mapping(data)
        return cls(
            uploaded=data["uploaded"],
            digests=list(data.get("digests", [])),
        )


@dataclass(frozen=True)
class OracleStatus:
    """The response returned by ``GET /oracle/status``."""

    active: bool
    epoch: int
    source_active: bool
    source_epoch: int

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Self:
        data = _mapping(data)
        return cls(
            active=bool(data.get("active", False)),
            epoch=_json_int(data.get("epoch")),
            source_active=bool(data.get("source_active", False)),
            source_epoch=_json_int(data.get("source_epoch")),
        )


@dataclass(frozen=True)
class ActivationResponse:
    """The response returned by ``POST /oracle/activate``."""

    status: str
    epoch: int

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Self:
        data = _mapping(data)
        return cls(
            status=str(data.get("status", "")),
            epoch=_json_int(data.get("epoch")),
        )

    @property
    def active(self) -> bool:
        return self.status == "active"


@dataclass(frozen=True)
class NodeRegistrationResponse:
    """The response returned by ``POST /nodes/register``."""

    status: str
    method: str | None = None
    node_id: str | None = None

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Self:
        data = _mapping(data)
        return cls(
            status=str(data.get("status", "")),
            method=data.get("method"),
            node_id=data.get("node_id"),
        )


@dataclass(frozen=True)
class PrepareResult:
    """The final state after ``NodeManager.prepare``."""

    node_id: str
    oracle: OracleStatus
    nodes: list[str]


def _strip_id_prefix(value: str, prefix: str) -> str:
    return value.strip().removeprefix(prefix)


def _mapping(data: Any) -> Mapping[str, Any]:
    if not isinstance(data, Mapping):
        raise TypeError(f"expected a JSON object, got {type(data).__name__}")
    return data


def _json_int(value: Any, default: int = 0) -> int:
    try:
        return int(value)
    except TypeError, ValueError:
        return default


def _validate_wait(timeout: float, poll_interval: float) -> None:
    if timeout <= 0:
        raise ValueError("timeout must be greater than zero")
    if poll_interval <= 0:
        raise ValueError("poll_interval must be greater than zero")


def _sleep_until(deadline: float, interval: float) -> None:
    remaining = deadline - time.monotonic()
    if remaining > 0:
        time.sleep(min(interval, remaining))


def _canonical_node_id(node_id: str | None) -> str | None:
    if not node_id:
        return None
    return f"Node-{_strip_id_prefix(node_id, 'Node-')}"


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
        self._node_id = _canonical_node_id(cli_args.node_id)

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

    def get_oracle_status(self) -> OracleStatus:
        """Return the search oracle's activation state."""

        response = httpx.get(
            self._url("/oracle/status"),
            headers=self._headers(),
            timeout=5.0,
        )
        if response.is_error:
            response.raise_for_status()
        return OracleStatus.from_dict(response.json())

    def activate_oracle(self) -> ActivationResponse:
        """Ask this node to activate the search oracle (idempotent)."""

        response = httpx.post(
            self._url("/oracle/activate"),
            headers=self._headers(),
            timeout=30.0,
        )
        if response.is_error:
            response.raise_for_status()
        return ActivationResponse.from_dict(response.json())

    def register_node(self) -> NodeRegistrationResponse:
        """Ask this node to self-register (idempotent)."""

        response = httpx.post(
            self._url("/nodes/register"),
            headers=self._headers(),
            timeout=60.0,
        )
        if response.is_error:
            response.raise_for_status()
        return NodeRegistrationResponse.from_dict(response.json())

    def list_nodes(self) -> list[str]:
        """Return the node IDs in the shared ``NodeRegistry``."""

        response = httpx.get(
            self._url("/nodes/list"),
            headers=self._headers(),
            timeout=5.0,
        )
        if response.is_error:
            response.raise_for_status()
        body = _mapping(response.json())
        return list(body.get("nodes", []))

    def ensure_oracle_active(
        self, *, activate: bool, timeout: float, poll_interval: float
    ) -> OracleStatus:
        """Wait until the search oracle is active.

        When ``activate`` is true, also POST to ``/oracle/activate`` with
        backoff while waiting. Use that only on a committee node.
        """

        _validate_wait(timeout, poll_interval)
        deadline = time.monotonic() + timeout
        next_post = time.monotonic()
        backoff = 2.0
        last_error = "no response"

        while time.monotonic() < deadline:
            try:
                status = self.get_oracle_status()
            except httpx.HTTPError as error:
                last_error = str(error)
            else:
                if status.active:
                    return status
                last_error = f"epoch {status.epoch} source_epoch {status.source_epoch}"

            if activate and time.monotonic() >= next_post:
                try:
                    self.activate_oracle()
                except httpx.HTTPStatusError as error:
                    if error.response.status_code == 503:
                        try:
                            body = error.response.json()
                        except ValueError:
                            body = {}
                        if (
                            isinstance(body, Mapping)
                            and body.get("status") == "not_staged"
                        ):
                            raise RuntimeError(
                                "search oracle is not staged; deploy with the "
                                "constructor committee"
                            ) from error
                    last_error = str(error)
                except httpx.HTTPError as error:
                    last_error = str(error)
                next_post = time.monotonic() + backoff
                backoff = min(backoff * 2, 30.0)

            _sleep_until(deadline, poll_interval)

        raise RuntimeError(
            f"search oracle did not become active within {timeout:.0f}s: {last_error}"
        )

    def ensure_registered(self, *, timeout: float, poll_interval: float) -> list[str]:
        """Wait until this node is in the shared ``NodeRegistry``."""

        if not self._node_id:
            raise ValueError("node id is required to prepare a node")
        _validate_wait(timeout, poll_interval)

        deadline = time.monotonic() + timeout
        next_post = time.monotonic()
        backoff = 2.0
        last_error = "no response"

        while time.monotonic() < deadline:
            # Poll the registry first: once the submitted registration lands,
            # there is no need to submit another certificate.
            try:
                nodes = self.list_nodes()
            except httpx.HTTPError as error:
                last_error = str(error)
            else:
                if self._node_id in nodes:
                    return nodes
                last_error = (
                    f"node {self._node_id} is not in the {len(nodes)} registered nodes"
                )

            if time.monotonic() >= next_post:
                try:
                    response = self.register_node()
                except httpx.HTTPError as error:
                    last_error = str(error)
                else:
                    if response.status not in ("registered", "submitted"):
                        last_error = (
                            f"unexpected registration status: {response.status}"
                        )
                next_post = time.monotonic() + backoff
                backoff = min(backoff * 2, 30.0)

            _sleep_until(deadline, poll_interval)

        raise RuntimeError(
            f"node was not registered within {timeout:.0f}s: {last_error}"
        )

    def prepare(
        self, *, activate_oracle: bool, timeout: float, poll_interval: float
    ) -> PrepareResult:
        """Wait for the oracle and self-register, returning the final state."""

        if not self._node_id or self._host is None:
            raise ValueError("node-id and lyquid-id are required to prepare a node")

        oracle = self.ensure_oracle_active(
            activate=activate_oracle,
            timeout=timeout,
            poll_interval=poll_interval,
        )
        nodes = self.ensure_registered(timeout=timeout, poll_interval=poll_interval)
        return PrepareResult(node_id=self._node_id, oracle=oracle, nodes=nodes)


__all__ = [
    "ActivationResponse",
    "NodeManager",
    "NodeRegistrationResponse",
    "OracleStatus",
    "PrepareResult",
    "SkillSummary",
    "UploadSkillResponse",
]
