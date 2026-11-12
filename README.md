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

Omit `--node-id` and `--lyquid-id` (or their env vars `MEAD_NODE_ID` / `MEAD_LYQUID_ID`) for public nodes.
Add `--skill-dir <SKILL_DIR>` to upload local skills alongside search.

## MCP Tools

- `search_skills(query, top_k=3)` - searches the network for skills using lexical BM25 matching. Returns skill summaries (name, description, digest) along with result counts and timeout status. For best results, include synonyms and related terms in your query; there is no stemming or synonym expansion. Run searches sequentially; concurrent calls may overwhelm the peer-to-peer network.
- `get_skill(digest, paths=None)` - fetches a skill by digest. Defaults to returning `SKILL.md` and a summary listing all available resources. Pass `paths=["file.txt", "README.md"]` to retrieve specific files. Resources are delivered as MCP embedded resources at `skill://<digest>/<path>`.

## Uploading Skills

Pass `--skill-dir <DIR>` to upload every skill in that directory when the MCP
starts. Omit it to run search-only. To upload without starting the server, use
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
