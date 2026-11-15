from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "prep_nodes.py"
SPEC = importlib.util.spec_from_file_location("prep_nodes", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
prep_nodes = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prep_nodes)


def _skills(count: int) -> list[Path]:
    return [Path(f"skill-{index}") for index in range(count)]


def test_parse_skipped_nodes() -> None:
    assert prep_nodes.parse_skipped_nodes("1,2") == {1, 2}
    assert prep_nodes.parse_skipped_nodes(" 3 ") == {3}
    assert prep_nodes.parse_skipped_nodes("") == set()


@pytest.mark.parametrize("value", ["0", "5", "one"])
def test_parse_skipped_nodes_rejects_invalid_indexes(value: str) -> None:
    with pytest.raises(argparse.ArgumentTypeError):
        prep_nodes.parse_skipped_nodes(value)


@pytest.mark.parametrize(
    ("num_nodes", "expected_sizes"),
    [(4, [5, 5, 5, 4]), (3, [7, 6, 6]), (1, [19])],
)
def test_select_fixture_skills_keeps_every_skill(
    monkeypatch: pytest.MonkeyPatch,
    num_nodes: int,
    expected_sizes: list[int],
) -> None:
    monkeypatch.setenv("MEAD_FIXTURE_SEED", "0")
    skills = _skills(19)

    assignments = prep_nodes.select_fixture_skills(skills, num_nodes)

    assert [len(assignment) for assignment in assignments] == expected_sizes
    assert sorted(skill for group in assignments for skill in group) == sorted(skills)


def test_assign_fixture_skills_leaves_skipped_nodes_empty(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("MEAD_FIXTURE_SEED", "0")
    nodes = [
        prep_nodes.Node(
            index=index,
            container=f"node{index}",
            node_id=f"Node-{index}",
            port=10087 + index,
        )
        for index in range(1, 5)
    ]

    assignments = prep_nodes.assign_fixture_skills(nodes, _skills(19), {1, 2})

    assert assignments[0] == []
    assert assignments[1] == []
    assert sorted(skill for group in assignments for skill in group) == sorted(
        _skills(19)
    )


def test_assign_fixture_skills_rejects_skipping_every_node() -> None:
    nodes = [
        prep_nodes.Node(
            index=index,
            container=f"node{index}",
            node_id=f"Node-{index}",
            port=10087 + index,
        )
        for index in range(1, 5)
    ]

    with pytest.raises(RuntimeError, match="all nodes are skipped"):
        prep_nodes.assign_fixture_skills(nodes, _skills(19), {1, 2, 3, 4})
