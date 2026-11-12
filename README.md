# Mead

Mead is a decentralized skill network for AI agents. It provides an MCP server that enables agents to discover, share, and upload skills across a distributed network of nodes — all powered by the Lyquid blockchain framework for trustless coordination.

With Mead, AI agents can search a global skill catalog, retrieve reusable capabilities, and contribute their own skills back to the network, creating a living ecosystem of agent knowledge.

## MCP Configuration

After installing [uv](https://docs.astral.sh/uv/), the stdio MCP server can be started with the following command:
```
uvx --from git+https://github.com/Davidyz/Mead.git mead
```

You should add the equivalent of this command to your agent's configuration.
For example, in pi-agent, use the following:
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
      "timeout": 120,
      "exposure": "direct"
    }
  }
}
```

This example is search-only: it does not pass `--skill-dir`, so the server does
not upload any local skills. To contribute skills, add `"--skill-dir"` and
`"<SKILL_DIR>"` to `args`.

`--node-id` and `--lyquid-id` can be omitted when `MEAD_NODE_ID` and
`MEAD_LYQUID_ID` are set in the environment.

## MCP Tools

- `search_skills(query, top_k=5)` — searches the network and returns an object
  with `skills` (name, description, digest), `timed_out`, received/expected and
  verified/rejected counts, and any `unresolved_digests`. Use the digest with
  `get_skill`. Matching is lexical BM25 with no stemming or synonyms, so
  include related words and alternative word forms in the query.
- `get_skill(digest, detail="skill_md", paths=None)` — fetches a skill by
  digest through `/skills/get`. Start with just `digest`: the default
  `detail="skill_md"` returns a summary block plus an embedded `SKILL.md`
  resource, and lists available resource paths without downloading them. Then
  pass `paths=[...]` to fetch only the resources you need. Files are returned as
  separate MCP embedded resources (`skill://<digest>/<path>`), so the skill's
  own Markdown is never wrapped or concatenated with synthetic headings.
  `detail="full"` includes every resource, which can be very large, so use it
  only when the entire skill is genuinely required.

## Uploading Skills

When `--skill-dir` is provided, the MCP uploads every skill under it when it
starts. Omit `--skill-dir` to run search-only without uploading anything. To run
only the upload step and exit, use `--upload-only` (which requires `--skill-dir`):

```bash
uv run mead \
  --upload-only \
  --server-url http://127.0.0.1:10087 \
  --skill-dir /path/to/skills \
  --node-id '<NODE_ID>' \
  --lyquid-id '<LYQUID_ID>'
```

The `Lyquid-` and `Node-` display prefixes are optional. The command builds the
virtual host header (`<lyquid-label>.<node-label>`) required to route requests
to a node directly, and prints how many local skills were loaded and uploaded
plus the node's current skill count.

## Preparing a Node for Search

`--prepare-only` uploads the local skills when `--skill-dir` is provided, waits
for the search oracle, asks the node to self-register, and exits without starting
the MCP. It is the per-node counterpart to the `prepare-nodes` devnet bootstrap:
the Lyquid must already be deployed with its constructor-staged committee.

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
