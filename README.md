# Mead

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
      "timeout": 30,
      "exposure": "direct"
    }
  }
}
```

## Uploading Skills

The MCP uploads every skill under `--skill-dir` when it starts. To run only the
upload step and exit, use `--upload-only`:

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
