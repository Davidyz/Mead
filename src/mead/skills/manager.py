import glob
from collections.abc import Iterable
from pathlib import Path
from typing import Literal

from .skill import Skill


class SkillManager:
    def __init__(self, skills_dir: Path | None = None):
        if skills_dir is not None and not skills_dir.is_dir():
            raise FileNotFoundError(f"{skills_dir} is not a directory.")
        self._skills_dir = skills_dir
        self._skills: dict[str, Skill] = {}

    def add_skill(self, skill: Skill) -> None:
        """Register a skill, ignoring duplicate content."""
        self._skills.setdefault(skill.canonical_key(), skill)

    def load_skills(self) -> None:
        if self._skills_dir is None:
            return

        for skill in glob.glob(
            str(self._skills_dir / "**" / "SKILL.md"), recursive=True
        ):
            skill_dir = Path(skill).parent
            try:
                parsed = Skill(skill_dir)
            except UnicodeDecodeError:
                # Canonical v1 resources must be UTF-8 text. Skip skills that
                # contain binary payloads until a bytes-backed resource exists.
                continue
            self.add_skill(parsed)

    def list_skill(self) -> list[dict[str, str]]:
        """List every skill name with its description."""
        return [
            {"name": skill.name, "description": skill.description}
            for skill in sorted(
                self._skills.values(),
                key=lambda item: (item.name, item.canonical_key()),
            )
        ]

    def get_skill(
        self,
        name: str,
        detail: Literal["description", "full"],
        paths: Iterable[str] | None = None,
    ) -> str:
        """Return a skill for an LLM agent, optionally including its files.

        ``detail="description"`` exposes only the description. ``detail="full"``
        exposes the instructions and the requested resource contents. Passing
        ``paths=None`` includes every resource; pass a subset to limit exposure.
        """

        skill = next(
            (
                candidate
                for candidate in self._skills.values()
                if candidate.name == name or candidate.id == name
            ),
            None,
        )
        if skill is None:
            raise ValueError(f"Could not find a skill named {name}")

        if detail == "description":
            return skill.description

        resources = {resource.path: resource for resource in skill.resources}
        selected = sorted(resources) if paths is None else list(dict.fromkeys(paths))
        missing = [path for path in selected if path not in resources]
        if missing:
            raise ValueError(
                f"Skill {name} does not contain resources: {', '.join(missing)}"
            )

        sections = [f"# {skill.name}", skill.description, skill.instructions]
        if resources:
            sections.append("## Available resources")
            sections.extend(f"- {path}" for path in sorted(resources))
        for path in selected:
            sections.append(f"## Resource: {path}\n\n{resources[path].contents}")
        return "\n\n".join(sections)
