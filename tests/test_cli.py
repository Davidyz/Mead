import asyncio

import mead
from mead.utils import CLIArgs


def test_mcp_startup_skips_uploading_without_a_skill_dir(
    monkeypatch,
) -> None:
    calls: list[str] = []

    class FakeNodeManager:
        def __init__(self, args: CLIArgs):
            assert args.skill_dir is None

        def upload_skill(self, _skill) -> None:
            raise AssertionError("upload_skill must not be called")

    class FakeServer:
        async def run_stdio_async(self) -> None:
            calls.append("server")

    class FakeTools:
        def __init__(self, node_manager: FakeNodeManager):
            pass

        def make_mcp_server(self) -> FakeServer:
            return FakeServer()

    def fail_skill_manager(_path) -> None:
        raise AssertionError("SkillManager must not be constructed")

    monkeypatch.setattr(mead, "load_dotenv", lambda: None)
    monkeypatch.setattr(
        mead,
        "parse_args",
        lambda: CLIArgs(
            server_url="http://127.0.0.1:10087",
            skill_dir=None,
            node_id="Node-example",
            lyquid_id="Lyquid-example",
        ),
    )
    monkeypatch.setattr(mead, "NodeManager", FakeNodeManager)
    monkeypatch.setattr(mead, "SkillManager", fail_skill_manager)
    monkeypatch.setattr(mead, "MeadTools", FakeTools)

    asyncio.run(mead.main_coro())

    assert calls == ["server"]
