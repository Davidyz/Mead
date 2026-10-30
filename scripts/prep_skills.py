import os
from pathlib import Path

TEST_DIR = Path(__file__).parent.parent / "tests"
DEFAULT_SKILLS_COMMIT = "dbd4588f9e1033efb41dad4bef2f7947c8993d44"


def clone_skills_repo(commit: str | None = None):
    cwd = os.getcwd()
    skills_dir = TEST_DIR / "skills"

    os.system(
        f"git clone https://github.com/anthropics/skills.git {skills_dir!s}"
    )

    os.chdir(skills_dir)
    if commit:
        os.system(f"git checkout {commit}")

    os.chdir(cwd)
    print(f"Demo skills are in {skills_dir.relative_to(TEST_DIR.parent) / 'skills'}")


def main():
    clone_skills_repo(os.environ.get("SKILLS_COMMIT", DEFAULT_SKILLS_COMMIT))

    # TODO: allocate the skills to nodes when we have a devnet docker-compose.yaml


if __name__ == "__main__":
    main()
