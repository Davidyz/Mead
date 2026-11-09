# Development Requirements

- [uv](https://docs.astral.sh/uv/) is required for managing the Python environment and running project commands such as `uv run`.

# Testing

The standard way to run tests for Lyquid is `shaker test Cargo.toml`.

For this app, some test cases are integration tests that require loading actual skills.
To prepare the skill catalog for those tests, run `make prepare-skills` before running the tests.

Run the Python tests with `make test`. This prepares the skill fixtures via
`make prepare-skills` first if they are not already present.

When a multi-node devnet has been prepared (see below), `make test` also runs the
search lifecycle integration test in `tests/test_search_lifecycle.py`. That test
drives `/search/new` → `/search/status` → `/search/get` against the devnet and is
skipped automatically when `tests/fixture/nodes.json` is missing or the devnet is
not reachable.

To run the lifecycle manually against a prepared devnet:

```bash
uv run python scripts/search_lifecycle.py --query "skill"
```

## Preparing a Multi-Node Devnet

`scripts/prep_nodes.py` is the full devnet bootstrap. It:

1. deploys Mead (unless `LYQUID_ID` is supplied),
2. creates per-node skill fixtures under `tests/fixture/node{1,2,3,4}/` and writes
   `tests/fixture/nodes.json` with the Lyquid/node IDs, ports, and selected fixture
   skill names,
3. uploads each node's fixtures,
4. activates the constructor-staged search oracle through `POST /oracle/activate`,
5. self-registers every node through `POST /nodes/register`, and
6. waits until the shared `NodeRegistry` contains all nodes.

It requires the stock multi-node Docker devnet from the LDK to be running:

```bash
docker compose -f ~/.shakenup/ldk/docker/multi/docker-compose.yaml up -d
```

Then run:

```bash
make prepare-nodes
```

There is no separate `make setup-search` step. The script funds each node's
submitter from node1's devnet account by default because self-registration is a
certified transaction. Use `--no-fund-nodes` to skip that, or `--funding-eth` /
`--devnet-dir` / `--anvil-container` to override the defaults.

For local debugging, the script also writes the deployed Lyquid and node1 IDs to
the repository-root `.env` (`MEAD_LYQUID_ID` and `MEAD_NODE_ID`). The file is
gitignored and is only a convenience for starting the MCP without repeating
flags; pass the IDs explicitly for other nodes or deployments.

To reuse an existing deployment instead of deploying again, pass its Lyquid ID:

```bash
make prepare-nodes LYQUID_ID=Lyquid-...
```

The script publishes the built Lyquid to the devnet's OCI registry so every
node can pull the image. The default reference is
`http://127.0.0.1:8000/lyquids:mead`; override it with the `LYQUID_REFERENCE`
environment variable if needed:

```bash
LYQUID_REFERENCE=http://127.0.0.1:8000/lyquids:my-tag make prepare-nodes
```

The script can also be run directly with `uv run python scripts/prep_nodes.py`.
It is idempotent: an already-active oracle and already-registered nodes
short-circuit without re-submitting certified calls.

After it completes, `GET /oracle/status` reports `{"active": true, "epoch": 1, ...}`
and `GET /nodes/list` contains every node ID. Instance endpoints are routed with
the virtual `Host` header `<lyquid-label>.<node-label>` (strip the `Lyquid-` and
`Node-` prefixes); the `scripts/search_lifecycle.py` and `tests/test_search_lifecycle.py`
helpers build it from `tests/fixture/nodes.json`.

# Creating a new Commit

When making a new commit, write a concise commit message that describes what the PR does.
The commit message should follow the conventional commit style with scoped prefixes like `feat: ` for new features, `fix: ` for bug fixes, etc.
You may pick the appropriate keyword for the commit.

Avoid using `git add -A` and always manually select files that should be added to the commit.
