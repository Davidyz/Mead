#!/usr/bin/env python3
"""Upload local skills to a running Mead Lyquid node.

Usage:
    python scripts/test_upload.py http://127.0.0.1:12345

The URL is normally the base URL printed by `shaker serve`. To bypass
`shaker serve` and call a node directly, pass `--host` with the Lyquid's
virtual host (`<lyquid-label>.<node-label>`). By default the script uploads
every skill under `tests/skills/skills` to `POST /skills/upload`, one skill per
request. Use `--path` to override the upload path, `--count` to query
`/skills/count`, and `--dry-run` to inspect the JSON payload without sending
it.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

import yaml

DEFAULT_SKILL_DIR = (
    Path(__file__).resolve().parent.parent / "tests" / "skills" / "skills"
)
KNOWN_FRONTMATTER_FIELDS = {
    "id",
    "name",
    "description",
    "triggers",
    "capabilities",
    "metadata",
}


def split_frontmatter(source: str) -> tuple[str, str]:
    """Split a SKILL.md file into YAML frontmatter and Markdown body."""
    content = source.lstrip()
    lines = content.splitlines(keepends=True)
    if not lines or lines[0].rstrip("\r\n") != "---":
        raise ValueError("missing YAML frontmatter")

    for index, line in enumerate(lines[1:], start=1):
        if line.rstrip("\r\n") == "---":
            return "".join(lines[1:index]), "".join(lines[index + 1 :])

    raise ValueError("missing closing frontmatter delimiter")


def read_resources(skill_dir: Path) -> list[dict[str, str]]:
    resources: list[dict[str, str]] = []
    for path in sorted(skill_dir.rglob("*")):
        relative = path.relative_to(skill_dir)
        if any(part.startswith(".") for part in relative.parts):
            continue
        if not path.is_file() or path.name.lower() == "skill.md":
            continue
        try:
            contents = path.read_text(encoding="utf-8")
        except UnicodeDecodeError as error:
            raise ValueError(
                f"resource {relative.as_posix()} is not UTF-8 text"
            ) from error
        resources.append({"path": relative.as_posix(), "contents": contents})
    return resources


def load_skill(skill_dir: Path) -> dict[str, Any]:
    manifest = skill_dir / "SKILL.md"
    matter, body = split_frontmatter(manifest.read_text(encoding="utf-8"))
    parsed = yaml.safe_load(matter)
    if not isinstance(parsed, dict):
        raise TypeError("frontmatter must be a mapping")

    name = parsed.get("name")
    description = parsed.get("description")
    if not isinstance(name, str) or not name:
        raise ValueError("frontmatter is missing a string `name`")
    if not isinstance(description, str):
        raise TypeError("frontmatter is missing a string `description`")

    identifier = parsed.get("id")
    if not isinstance(identifier, str) or not identifier.strip():
        identifier = name

    metadata = parsed.get("metadata", {})
    if not isinstance(metadata, dict):
        raise TypeError("`metadata` must be a mapping of strings to strings")
    if any(
        not isinstance(key, str) or not isinstance(value, str)
        for key, value in metadata.items()
    ):
        raise TypeError("`metadata` must be a mapping of strings to strings")

    extra = {
        key: value
        for key, value in parsed.items()
        if key not in KNOWN_FRONTMATTER_FIELDS
    }

    return {
        "id": identifier,
        "name": name,
        "description": description,
        "triggers": parsed.get("triggers", []),
        "instructions": body,
        "capabilities": parsed.get("capabilities", []),
        "metadata": metadata,
        "extra": extra,
        "resources": read_resources(skill_dir),
    }


def discover_skills(skill_dir: Path, limit: int | None) -> list[dict[str, Any]]:
    skills: list[dict[str, Any]] = []
    for manifest in sorted(skill_dir.rglob("SKILL.md")):
        try:
            skills.append(load_skill(manifest.parent))
        except (OSError, TypeError, ValueError, yaml.YAMLError) as error:
            print(f"skipping {manifest.parent}: {error}", file=sys.stderr)
            continue
        if limit is not None and len(skills) >= limit:
            break

    if not skills:
        raise SystemExit(f"no uploadable skills found under {skill_dir}")
    return skills


def request_headers(
    host: str | None, *, content_type: str | None = None
) -> dict[str, str]:
    headers = {"accept": "application/json"}
    if content_type is not None:
        headers["content-type"] = content_type
    if host is not None:
        headers["host"] = host
    return headers


def get_skill_count(
    base_url: str, count_path: str, timeout: float, host: str | None
) -> None:
    url = base_url.rstrip("/") + "/" + count_path.lstrip("/")
    request = Request(url, method="GET", headers=request_headers(host))
    try:
        with urlopen(request, timeout=timeout) as response:
            status = response.status
            response_body = response.read().decode("utf-8", errors="replace")
    except HTTPError as error:
        status = error.code
        response_body = error.read().decode("utf-8", errors="replace")
    except URLError as error:
        raise SystemExit(f"could not reach {url}: {error.reason}") from error

    print(f"GET {url} -> {status}")
    print(response_body)
    if not 200 <= status < 300:
        raise SystemExit(1)


def encode_skill(skill: dict[str, Any]) -> bytes:
    return json.dumps(skill, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def post_skill(
    url: str,
    skill: dict[str, Any],
    index: int,
    total: int,
    timeout: float,
    host: str | None,
) -> dict[str, Any] | None:
    body = encode_skill(skill)
    name = skill.get("name", "<unknown>")
    request = Request(
        url,
        data=body,
        method="POST",
        headers=request_headers(host, content_type="application/json"),
    )

    try:
        with urlopen(request, timeout=timeout) as response:
            status = response.status
            response_body = response.read().decode("utf-8", errors="replace")
    except HTTPError as error:
        status = error.code
        response_body = error.read().decode("utf-8", errors="replace")
    except URLError as error:
        raise SystemExit(f"could not reach {url}: {error.reason}") from error

    print(
        f"[{index}/{total}] POST {url} skill={name!r} ({len(body)} bytes) -> {status}"
    )
    print(response_body)
    if not 200 <= status < 300:
        raise SystemExit(1)

    try:
        decoded = json.loads(response_body)
    except json.JSONDecodeError:
        return None
    return decoded if isinstance(decoded, dict) else None


def upload(
    base_url: str,
    path: str,
    skills: list[dict[str, Any]],
    timeout: float,
    host: str | None,
) -> None:
    url = base_url.rstrip("/") + "/" + path.lstrip("/")
    uploaded = 0

    for index, skill in enumerate(skills, start=1):
        decoded = post_skill(url, skill, index, len(skills), timeout, host)
        if decoded is not None and isinstance(decoded.get("uploaded"), int):
            uploaded += decoded["uploaded"]

    print(f"registered {uploaded} skill(s) across {len(skills)} request(s)")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "url",
        nargs="?",
        default="http://127.0.0.1:8081/",
        help="Base URL printed by `shaker serve` (default: %(default)s)",
    )
    parser.add_argument(
        "--path",
        default="/skills/upload",
        help="Upload endpoint path (default: %(default)s)",
    )
    parser.add_argument(
        "--host",
        help="Virtual Host header for direct node access: <lyquid-label>.<node-label>",
    )
    parser.add_argument(
        "--skill-dir",
        type=Path,
        default=DEFAULT_SKILL_DIR,
        help="Directory containing skill subdirectories (default: %(default)s)",
    )
    parser.add_argument(
        "--limit",
        type=int,
        help="Upload at most this many skills",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=30.0,
        help="HTTP timeout in seconds (default: %(default)s)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the JSON payload without sending it",
    )
    parser.add_argument(
        "--count",
        action="store_true",
        help="Query the skill count instead of uploading",
    )
    parser.add_argument(
        "--count-path",
        default="/skills/count",
        help="Skill count endpoint path (default: %(default)s)",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.count:
        get_skill_count(args.url, args.count_path, args.timeout, args.host)
        return

    if not args.skill_dir.is_dir():
        raise SystemExit(
            f"{args.skill_dir} is not a directory; run `make prepare` first"
        )

    skills = discover_skills(args.skill_dir, args.limit)
    print(f"prepared {len(skills)} skill(s) from {args.skill_dir}")
    if args.dry_run:
        print(json.dumps(skills, indent=2, ensure_ascii=False))
        return

    upload(args.url, args.path, skills, args.timeout, args.host)


if __name__ == "__main__":
    main()
