from __future__ import annotations

import pytest

from mead.utils import parse_args


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
