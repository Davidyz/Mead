import glob
from pathlib import Path

from .skill import Skill


class SkillManager:
    def __init__(self, skills_dir: Path | None = None):
        if skills_dir is not None and not skills_dir.is_dir():
            raise FileNotFoundError(f"{skills_dir} is not a directory.")
        self._skills_dir = skills_dir
        self._skills: dict[str, Skill] = {}

    def add_skill(self, skill: Skill) -> None:
        """Register a skill regardless of where it was loaded from."""
        self._skills[skill.id] = skill

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
