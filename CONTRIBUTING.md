# Development

This guide covers the developer workflow: the toolchain, testing, and the
advanced options for the devnet bootstrap. Basic devnet setup — installing the
MCP server, `make start-devnet`, and `make prepare-nodes` — lives in the README
under [Deploying a Devnet](./README.md#deploying-a-devnet-development-only).

Building, testing, and deploying Mead requires the Lyquid Development Kit (LDK).
End users consume Mead as an MCP server and never need the LDK or Docker.

## Toolchain

Install the LDK, which provides `shaker`, Rust, Foundry, and the local devnets,
by following the official documentation at <https://docs.lyquor.dev/docs/setup/>.

The `lyquid` crate in `Cargo.toml` must match the installed LDK release; run
`make check-ldk` after installing or upgrading, and treat a mismatch as the
first thing to fix when a build or deploy fails.

Docker Engine with Docker Compose v2 is required only for the multi-node devnet
used by the integration tests (`make start-devnet`). Unit tests run without it.
The devnet definition lives at
`~/.shakenup/ldk/docker/multi/docker-compose.yaml`; override the location with
`LYQUOR_DEVNET_DIR` if the LDK was installed elsewhere.

## Testing

The standard way to run tests for Lyquid is `shaker test Cargo.toml`.

For this app, some test cases are integration tests that require loading actual skills.
To prepare the skill catalog for those tests, run `make prepare-skills` before running the tests.

Run the Python tests with `make test`. This prepares the skill fixtures via
`make prepare-skills` first if they are not already present.

When a multi-node devnet has been prepared (see [Devnet Options](#devnet-options)
below), `make test` also runs the search lifecycle integration test in
`tests/test_search_lifecycle.py`. That test drives `/search/new` →
`/search/status` → `/search/get` against the devnet and is skipped automatically
when `tests/fixture/nodes.json` is missing or the devnet is not reachable.

To run the lifecycle manually against a prepared devnet:

```bash
uv run python scripts/search_lifecycle.py --query "skill"
```

## Devnet Options

`scripts/prep_nodes.py` is the full devnet bootstrap behind
`make prepare-nodes`. It:

1. deploys Mead (unless `LYQUID_ID` is supplied),
2. creates per-node skill fixtures under `tests/fixture/node{1,2,3,4}/` and writes
   `tests/fixture/nodes.json` with the Lyquid/node IDs, ports, and selected fixture
   skill names,
3. uploads each node's fixtures,
4. activates the constructor-staged search oracle through `POST /oracle/activate`,
5. self-registers every node through `POST /nodes/register`, and
6. waits until the shared `NodeRegistry` contains all nodes.

It requires the stock multi-node Docker devnet from the LDK to be running;
`make start-devnet` is a convenience wrapper around the LDK's `docker compose`
setup. The script is idempotent: an already-active oracle and already-registered
nodes short-circuit without re-submitting certified calls.

The script funds each node's submitter from node1's devnet account by default 
because self-registration is a certified transaction. Use `--no-fund-nodes` to skip 
that, or `--funding-eth` / `--devnet-dir` / `--anvil-container` to override the 
defaults.

After it completes, `GET /oracle/status` reports `{"active": true, "epoch": 1, ...}`
and `GET /nodes/list` contains every node ID. Instance endpoints are routed with
the virtual `Host` header `<lyquid-label>.<node-label>` (strip the `Lyquid-` and
`Node-` prefixes); the `scripts/search_lifecycle.py` and `tests/test_search_lifecycle.py`
helpers build it from `tests/fixture/nodes.json`.

The complete HTTP contract — paths, methods, status codes, and response
bodies — is documented in the comments next to each
`#[method::instance(export = http, ...)]` handler in the Rust source (see
`src/lib.rs`).

# Creating a new Commit

When making a new commit, write a concise commit message that describes what the PR does.
The commit message should follow the conventional commit style with scoped prefixes like `feat: ` for new features, `fix: ` for bug fixes, etc.
You may pick the appropriate keyword for the commit.

Avoid using `git add -A` and always manually select files that should be added to the commit.
