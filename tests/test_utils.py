from __future__ import annotations

from pathlib import Path

import pytest

from mead.utils import parse_args


@pytest.fixture(autouse=True)
def clear_id_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("MEAD_NODE_ID", raising=False)
    monkeypatch.delenv("MEAD_LYQUID_ID", raising=False)


def _base_args(*extra: str) -> list[str]:
    return [
        "--server-url",
        "http://127.0.0.1:10087",
        "--skill-dir",
        "tests/fixture/node1",
        "--node-id",
        "Node-example",
        "--lyquid-id",
        "Lyquid-example",
        *extra,
    ]


def test_ids_fall_back_to_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("MEAD_NODE_ID", "Node-from-env")
    monkeypatch.setenv("MEAD_LYQUID_ID", "Lyquid-from-env")

    args = parse_args(["--skill-dir", "tests/fixture/node1"])

    assert args.node_id == "Node-from-env"
    assert args.lyquid_id == "Lyquid-from-env"


def test_explicit_ids_override_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("MEAD_NODE_ID", "Node-from-env")
    monkeypatch.setenv("MEAD_LYQUID_ID", "Lyquid-from-env")

    args = parse_args(
        [
            "--skill-dir",
            "tests/fixture/node1",
            "--node-id",
            "Node-flag",
            "--lyquid-id",
            "Lyquid-flag",
        ]
    )

    assert args.node_id == "Node-flag"
    assert args.lyquid_id == "Lyquid-flag"


def test_prepare_only_flags_are_parsed() -> None:
    args = parse_args(
        _base_args(
            "--prepare-only",
            "--activate-oracle",
            "--wait-timeout",
            "30",
            "--poll-interval",
            "0.5",
        )
    )

    assert args.prepare_only is True
    assert args.upload_only is False
    assert args.activate_oracle is True
    assert args.wait_timeout == 30.0
    assert args.poll_interval == 0.5


def test_upload_only_and_prepare_only_are_mutually_exclusive() -> None:
    with pytest.raises(SystemExit):
        parse_args(_base_args("--upload-only", "--prepare-only"))


def test_activate_oracle_requires_prepare_only() -> None:
    with pytest.raises(SystemExit):
        parse_args(_base_args("--activate-oracle"))


@pytest.mark.parametrize(
    "flags",
    [
        ("--wait-timeout", "0"),
        ("--poll-interval", "0"),
        ("--poll-interval", "-1"),
    ],
)
def test_prepare_only_rejects_non_positive_intervals(flags: tuple[str, str]) -> None:
    with pytest.raises(SystemExit):
        parse_args(_base_args("--prepare-only", *flags))


@pytest.mark.parametrize("missing", ["--node-id", "--lyquid-id"])
def test_required_arguments_are_validated(missing: str) -> None:
    args = _base_args()
    index = args.index(missing)
    del args[index : index + 2]
    with pytest.raises(SystemExit):
        parse_args(args)


def test_skill_dir_is_optional_for_the_mcp() -> None:
    args = parse_args(["--node-id", "Node-example", "--lyquid-id", "Lyquid-example"])

    assert args.skill_dir is None
    assert args.upload_only is False


def test_skill_dir_is_optional_for_prepare_only() -> None:
    args = parse_args(
        [
            "--prepare-only",
            "--node-id",
            "Node-example",
            "--lyquid-id",
            "Lyquid-example",
        ]
    )

    assert args.skill_dir is None
    assert args.prepare_only is True


def test_upload_only_requires_a_skill_dir() -> None:
    with pytest.raises(SystemExit):
        parse_args(
            [
                "--upload-only",
                "--node-id",
                "Node-example",
                "--lyquid-id",
                "Lyquid-example",
            ]
        )


def test_skill_dir_is_parsed_as_a_path() -> None:
    args = parse_args(_base_args())

    assert args.skill_dir == Path("tests/fixture/node1")
