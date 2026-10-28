import glob
from pathlib import Path

from .skill import Skill


class SkillManager:
    def __init__(self, skills_dir: Path):
        if not skills_dir.is_dir():
            raise FileNotFoundError(f"{skills_dir} is not a directory.")
        self._skills_dir = skills_dir
        self._skills: dict[Path, Skill] = {}

    def load_skills(self):
        for skill in glob.glob(
            str(self._skills_dir / "**" / "SKILL.md"), recursive=True
        ):
            skill_dir = Path(skill).parent
            try:
                self._skills[skill_dir] = Skill(skill_dir)
            except UnicodeDecodeError:
                # Canonical v1 resources must be UTF-8 text. Skip skills that
                # contain binary payloads until a bytes-backed resource exists.
                continue
