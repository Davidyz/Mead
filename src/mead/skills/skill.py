import json
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Self

lazy import frontmatter

_KNOWN_FRONTMATTER_FIELDS = {
    "id",
    "name",
    "description",
    "triggers",
    "capabilities",
    "metadata",
}


@dataclass
class SkillResource:
    """A resource file shipped with a skill.

    ``path`` is relative to the skill directory and always uses ``/`` as the
    separator, matching the Rust representation.
    """

    path: str
    contents: str

    def to_dict(self) -> dict[str, str]:
        return {"path": self.path, "contents": self.contents}


@dataclass
class Frontmatter:
    """Metadata parsed from the YAML frontmatter of a ``SKILL.md`` file."""

    id: str | None = None
    name: str = ""
    description: str = ""
    triggers: list[str] = field(default_factory=list)
    capabilities: list[str] = field(default_factory=list)
    metadata: dict[str, str] = field(default_factory=dict)
    extra: dict[str, Any] = field(default_factory=dict)


def _expect_string(value: Any, field_name: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{field_name} must be a string")
    return value


def _expect_string_list(value: Any, field_name: str) -> list[str]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise TypeError(f"{field_name} must be a list of strings")
    return list(value)


def _expect_string_map(value: Any, field_name: str) -> dict[str, str]:
    if not isinstance(value, dict):
        raise TypeError(f"{field_name} must be an object with string values")
    if any(
        not isinstance(key, str) or not isinstance(item, str)
        for key, item in value.items()
    ):
        raise TypeError(f"{field_name} must be an object with string keys and values")
    return dict(value)


def _validate_resource_path(path: str) -> None:
    if (
        not path
        or path.startswith("/")
        or "\\" in path
        or any(part in ("", ".", "..") for part in path.split("/"))
    ):
        raise ValueError(f"invalid resource path: {path}")


def _split_frontmatter(source: str) -> tuple[str, str]:
    """Split a Markdown document into YAML frontmatter and body.

    The body is preserved exactly after the closing delimiter, matching the
    Rust ``markdown-frontmatter`` parser used by the on-chain implementation.
    """

    content = source.lstrip()
    lines = content.splitlines(keepends=True)
    if not lines or lines[0].rstrip("\r\n") != "---":
        raise ValueError("SKILL.md is missing YAML frontmatter")

    for index, line in enumerate(lines[1:], start=1):
        if line.rstrip("\r\n") == "---":
            return "".join(lines[1:index]), "".join(lines[index + 1 :])

    raise ValueError("SKILL.md has no closing frontmatter delimiter")


def _parse_frontmatter(source: str) -> tuple[Frontmatter, str]:
    matter, body = _split_frontmatter(source)

    try:
        parsed = frontmatter.loads(f"---\n{matter}---\n").metadata
    except Exception as error:
        raise ValueError(f"invalid skill frontmatter: {error}") from error

    if not isinstance(parsed, dict):
        raise TypeError("skill frontmatter must be a mapping")

    if "name" not in parsed:
        raise ValueError("skill frontmatter is missing `name`")
    if "description" not in parsed:
        raise ValueError("skill frontmatter is missing `description`")

    identifier = parsed.get("id")
    if identifier is not None:
        identifier = _expect_string(identifier, "id")

    metadata = _expect_string_map(parsed.get("metadata", {}), "metadata")
    extra = {
        key: value
        for key, value in parsed.items()
        if key not in _KNOWN_FRONTMATTER_FIELDS
    }

    return (
        Frontmatter(
            id=identifier,
            name=_expect_string(parsed["name"], "name"),
            description=_expect_string(parsed["description"], "description"),
            triggers=_expect_string_list(parsed.get("triggers", []), "triggers"),
            capabilities=_expect_string_list(
                parsed.get("capabilities", []), "capabilities"
            ),
            metadata=metadata,
            extra=extra,
        ),
        body,
    )


class Skill:
    """A skill parsed from a directory containing a ``SKILL.md`` manifest."""

    def __init__(self, skill_dir: Path):
        skill_dir = Path(skill_dir)
        if not (skill_dir / "SKILL.md").is_file():
            raise ValueError(
                f"{skill_dir} does not contain a SKILL.md file. It's probably not a skill."
            )

        self._dir = skill_dir
        self.parse_frontmatter()
        self.resources = self._get_resources()

    @classmethod
    def from_dir(cls, skill_dir: Path) -> Self:
        return cls(skill_dir)

    @classmethod
    def _from_parts(
        cls,
        frontmatter: Frontmatter,
        instructions: str,
        resources: list[SkillResource],
        skill_dir: Path | None = None,
    ) -> Self:
        skill = cls.__new__(cls)
        skill._dir = skill_dir
        skill._frontmatter = frontmatter
        skill.id = (
            frontmatter.id
            if frontmatter.id is not None and frontmatter.id.strip()
            else frontmatter.name
        )
        skill.name = frontmatter.name
        skill.description = frontmatter.description
        skill.triggers = list(frontmatter.triggers)
        skill.instructions = instructions
        skill.capabilities = list(frontmatter.capabilities)
        skill.metadata = dict(frontmatter.metadata)
        skill.extra = dict(frontmatter.extra)
        skill.resources = resources
        skill._validate_resources()
        return skill

    @classmethod
    def from_manifest(
        cls,
        frontmatter: Frontmatter | Mapping[str, Any],
        instructions: str,
        resources: list[SkillResource | Mapping[str, str]] | None = None,
    ) -> Self:
        """Build a skill from already-parsed frontmatter and instructions."""

        if isinstance(frontmatter, Frontmatter):
            parsed = frontmatter
        else:
            parsed = _frontmatter_from_mapping(frontmatter, flatten_extra=True)

        parsed_resources: list[SkillResource] = []
        for resource in resources or []:
            if isinstance(resource, SkillResource):
                parsed_resources.append(resource)
            else:
                parsed_resources.append(_resource_from_mapping(resource))

        return cls._from_parts(parsed, instructions, parsed_resources)

    def parse_frontmatter(self) -> Frontmatter:
        source = (self._dir / "SKILL.md").read_text(encoding="utf-8")
        frontmatter, body = _parse_frontmatter(source)

        self._frontmatter = frontmatter
        self.id = (
            frontmatter.id
            if frontmatter.id is not None and frontmatter.id.strip()
            else frontmatter.name
        )
        self.name = frontmatter.name
        self.description = frontmatter.description
        self.triggers = list(frontmatter.triggers)
        self.instructions = body
        self.capabilities = list(frontmatter.capabilities)
        self.metadata = dict(frontmatter.metadata)
        self.extra = dict(frontmatter.extra)
        return frontmatter

    def _get_resources(self) -> list[SkillResource]:
        resources: list[SkillResource] = []

        for path in self._dir.rglob("*"):
            relative = path.relative_to(self._dir)
            if any(part.startswith(".") for part in relative.parts):
                continue
            if not path.is_file() or path.name.lower() == "skill.md":
                continue

            resources.append(
                SkillResource(
                    path=relative.as_posix(),
                    contents=path.read_text(encoding="utf-8"),
                )
            )

        resources.sort(key=lambda resource: resource.path)
        return resources

    def _validate_resources(self) -> None:
        seen: set[str] = set()
        for resource in self.resources:
            _validate_resource_path(resource.path)
            if resource.path in seen:
                raise ValueError(f"duplicate resource path: {resource.path}")
            seen.add(resource.path)

    def to_dict(self) -> dict[str, Any]:
        """Return the JSON transport representation of this skill."""

        return {
            "id": self.id,
            "name": self.name,
            "description": self.description,
            "triggers": list(self.triggers),
            "instructions": self.instructions,
            "capabilities": list(self.capabilities),
            "metadata": dict(self.metadata),
            "extra": dict(self.extra),
            "resources": [resource.to_dict() for resource in self.resources],
        }

    def to_json(self) -> str:
        """Encode the skill using the shared Rust/Python JSON schema."""

        return json.dumps(
            self.to_dict(),
            ensure_ascii=False,
            separators=(",", ":"),
        )

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Self:
        return cls._from_parts(
            _frontmatter_from_mapping(data),
            _expect_string(data["instructions"], "instructions"),
            [
                _resource_from_mapping(resource)
                for resource in data.get("resources", [])
            ],
        )

    @classmethod
    def from_json(cls, data: str | bytes | bytearray) -> Self:
        try:
            decoded = json.loads(data)
        except (TypeError, json.JSONDecodeError) as error:
            raise ValueError(f"invalid skill JSON: {error}") from error

        if not isinstance(decoded, dict):
            raise TypeError("skill JSON must be an object")
        return cls.from_dict(decoded)

    @classmethod
    def from_json_slice(cls, data: str | bytes | bytearray) -> Self:
        return cls.from_json(data)


def _resource_from_mapping(resource: Mapping[str, Any]) -> SkillResource:
    if not isinstance(resource, Mapping):
        raise TypeError("resource must be an object")
    if "path" not in resource or "contents" not in resource:
        raise ValueError("resource must contain `path` and `contents`")

    parsed = SkillResource(
        path=_expect_string(resource["path"], "resource.path"),
        contents=_expect_string(resource["contents"], "resource.contents"),
    )
    _validate_resource_path(parsed.path)
    return parsed


def _frontmatter_from_mapping(
    data: Mapping[str, Any], *, flatten_extra: bool = False
) -> Frontmatter:
    if not isinstance(data, Mapping):
        raise TypeError("skill must be an object")
    if "name" not in data:
        raise ValueError("skill is missing `name`")
    if "description" not in data:
        raise ValueError("skill is missing `description`")

    identifier = data.get("id")
    if identifier is not None:
        identifier = _expect_string(identifier, "id")

    metadata = _expect_string_map(data.get("metadata", {}), "metadata")
    extra = data.get("extra", {})
    if not isinstance(extra, Mapping):
        raise TypeError("extra must be an object")
    extra = dict(extra)
    if flatten_extra:
        extra = {
            **{
                key: value
                for key, value in data.items()
                if key not in _KNOWN_FRONTMATTER_FIELDS and key != "extra"
            },
            **extra,
        }

    return Frontmatter(
        id=identifier,
        name=_expect_string(data["name"], "name"),
        description=_expect_string(data["description"], "description"),
        triggers=_expect_string_list(data.get("triggers", []), "triggers"),
        capabilities=_expect_string_list(data.get("capabilities", []), "capabilities"),
        metadata=metadata,
        extra=extra,
    )
