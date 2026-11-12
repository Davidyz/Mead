# Mead

## MCP Configuration

After installing [uv](https://docs.astral.sh/uv/), the stdio MCP server can be started with:
```
uvx --from git+https://github.com/Davidyz/Mead.git mead
```

Add it to your agent's MCP configuration:
```json
{
  "mcpServers": {
    "mead": {
      "command": "uvx",
      "args": [
        "--from",
        "git+https://github.com/Davidyz/Mead.git",
        "mead",
        "--node-id",
        "<NODE_ID>",
        "--lyquid-id",
        "<LYQUID_ID>"
      ],
      "timeout": 120
    }
  }
}
```

Pass `--node-id` and `--lyquid-id` (or set `MEAD_NODE_ID` / `MEAD_LYQUID_ID` env vars) to connect to a specific node.
Add `--skill-dir <SKILL_DIR>` to upload local skills alongside search.

## MCP Tools

- `search_skills(query, top_k=3)` - searches the network for skills using lexical BM25 matching. Returns skill summaries (name, description, digest) along with result counts and timeout status. For best results, include synonyms and related terms in your query; there is no stemming or synonym expansion. Run searches sequentially; concurrent calls may overwhelm the peer-to-peer network.
- `get_skill(digest, paths=None)` - fetches a skill by digest. Defaults to returning `SKILL.md` and a summary listing all available resources. Pass `paths=["file.txt", "README.md"]` to retrieve specific files. Resources are delivered as MCP embedded resources at `skill://<digest>/<path>`.

## Uploading Skills

Pass `--skill-dir <DIR>` to upload every skill in that directory once at
startup. The MCP server then runs for both search and skill retrieval.
Omit `--skill-dir` to run search-only.

To upload skills once and exit (without starting the MCP server), add
`--upload-only`:

```bash
uv run mead \
  --upload-only \
  --server-url http://127.0.0.1:10087 \
  --skill-dir /path/to/skills \
  --node-id '<NODE_ID>' \
  --lyquid-id '<LYQUID_ID>'
```

Display prefixes (`Lyquid-`, `Node-`) are optional in node IDs. The command
prints how many skills were uploaded and the node's current total.

## Preparing Your Node

Before the MCP can participate in network searches, the node must be registered
in the shared node registry. Run `--prepare-only` to upload skills, wait for
the search oracle to become active, and self-register:

```bash
uv run mead \
  --prepare-only \
  --server-url http://127.0.0.1:10087 \
  --skill-dir /path/to/skills \
  --node-id '<NODE_ID>' \
  --lyquid-id '<LYQUID_ID>'
```

If this is the first committee node in your network, also pass `--activate-oracle`
to activate the search oracle. All other nodes omit this flag and wait for the
oracle to activate automatically.

Useful flags:

- `--wait-timeout` (default `120`): seconds to wait for oracle activation and
  node registration.
- `--poll-interval` (default `1.0`): seconds between status checks.

The command is idempotent — safe to rerun. It exits `0` when the node is ready,
and non-zero on timeout or HTTP error. `--prepare-only` and `--upload-only` are
mutually exclusive.
