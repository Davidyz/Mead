from mcp.server import MCPServer


class NodeManager:
    def __init__(self, node_url: str):
        self._node_url = node_url

    async def search(query: str, top_k: int = 3) -> list[str]:
        return []

    def export_mcp(self):
        server = MCPServer("mead")
        server.add_tool(self.search)
