# Development Requirements

- [uv](https://docs.astral.sh/uv/) is required for managing the Python environment and running project commands such as `uv run`.

# Testing

The standard way to run tests for Lyquid is `shaker test Cargo.toml`.

For this app, some test cases are integration tests that require loading actual skills.
To prepare the skill catalog for those tests, run `make prepare-skills` before running the tests.

Run the Python unit tests with `make test`. This downloads the skill fixtures via
`make prepare-skills` first if they are not already present.

## Preparing a Multi-Node Devnet

`scripts/prep_nodes.py` deploys Mead, creates per-node skill fixtures under
`tests/fixture/node{1,2,3,4}/`, writes `tests/fixture/nodes.json` with the
Lyquid/node IDs, ports, and selected fixture skill names, uploads the fixtures,
and prints the skills each node reports. It requires the stock multi-node Docker
devnet from the LDK to be running:

```bash
docker compose -f ~/.shakenup/ldk/docker/multi/docker-compose.yaml up -d
```

Then run:

```bash
make prepare-nodes
```

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

# Creating a new Commit

When making a new commit, write a concise commit message that describes what the PR does.
The commit message should follow the conventional commit style with scoped prefixes like `feat: ` for new features, `fix: ` for bug fixes, etc.
You may pick the appropriate keyword for the commit.

Avoid using `git add -A` and always manually select files that should be added to the commit.
