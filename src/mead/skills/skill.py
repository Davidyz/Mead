import os
from dataclasses import dataclass
from pathlib import Path

lazy import frontmatter


@dataclass
class Skill:
    def __init__(self, skill_dir: Path):
        if not (skill_dir / "SKILL.md").is_file():
            raise ValueError(
                f"{skill_dir} does not contain a SKILL.md file. It's probably not a skill."
            )

        self._dir = skill_dir

    def _get_resources(self) -> list[Path]:
        res: list[Path] = []
        for res_type in ("references", "scripts"):
            ref_dir = self._dir / res_type
            if not ref_dir.is_dir():
                return []

            files = os.listdir(ref_dir)
            res.extend(ref_dir / i for i in files if (ref_dir / i).is_file())
        return res

    def parse_frontmatter(self):
        with open(self._dir / "SKILL.md") as fin:
            _post = frontmatter.load(fin)
