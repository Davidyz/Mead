# Development Requirements

- [uv](https://docs.astral.sh/uv/) is required for managing the Python environment and running project commands such as `uv run`.

## Running the Local MCP for Debugging

For debugging and experimenting, you can run the MCP server directly from the local source instead of pulling it from the remote repository. Add a configuration like the following to your agent's MCP settings:

```json
{
  "mcpServers": {
    "mead": {
      "command": "uv",
      "args": [
        "run",
        "mead",
        "--server-url",
        "http://127.0.0.1:10087",
        "--skill-dir",
        "/path/to/Mead/tests/fixture/node1"
      ],
      "timeout": 120
    }
  }
}
```

Key differences from the production setup:
- Uses `uv run mead` (local source) instead of `uvx --from git+...` (remote package)
- Passes `--skill-dir` to upload local skill fixtures for testing
- `--server-url` points to your local devnet node

Use `--node-id` and `--lyquid-id` flags (or set `MEAD_NODE_ID` / `MEAD_LYQUID_ID` env vars) if your node requires authentication.

## Preparing a Node for Search

`--prepare-only` uploads local skills (when `--skill-dir` is provided), waits for
the search oracle, self-registers the node, and exits without starting the MCP.
It is the per-node counterpart to the `prepare-nodes` devnet bootstrap: the
Lyquid must already be deployed with its constructor-staged committee.

On a committee node (and only there), let the MCP drive activation:

```bash
uv run mead \
  --prepare-only \
  --activate-oracle \
  --server-url http://127.0.0.1:10087 \
  --skill-dir /path/to/skills \
  --node-id '<NODE_ID>' \
  --lyquid-id '<LYQUID_ID>'
```

On every other node, omit `--activate-oracle`; the command waits for the oracle
to become active, then self-registers:

```bash
uv run mead \
  --prepare-only \
  --server-url http://127.0.0.1:11087 \
  --skill-dir /path/to/skills \
  --node-id '<NODE_ID>' \
  --lyquid-id '<LYQUID_ID>'
```

Useful flags:

- `--wait-timeout` (default `120`): seconds to wait for oracle activation and,
  separately, for the `NodeRegistry` to include the node.
- `--poll-interval` (default `1.0`): seconds between status checks.

The command is idempotent and safe to rerun: an active oracle and a registered
node return immediately. It exits `0` when the node is ready, and non-zero with
the last HTTP error/timeout otherwise. `--upload-only` and `--prepare-only` are
mutually exclusive; `--activate-oracle` requires `--prepare-only`.

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

To test with some nodes contributing no skills, pass their indexes to
`SKIP_UPLOAD_NODES`. All supported skills are redistributed across the remaining
nodes, so nothing is dropped:

```bash
SKIP_UPLOAD_NODES=1,2 make prepare-nodes
```

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
