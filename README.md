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
