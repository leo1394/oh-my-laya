import asyncio
import os
import sys

from mcp import Client, StdioServerParameters


async def verify(server: str):
    params = StdioServerParameters(
        command=server,
        env={"LAYA_MODEL_DIR": "/tmp/not-loaded-during-discovery"},
    )
    async with Client(params) as client:
        tools = await client.list_tools()
        names = [tool.name for tool in tools.tools]
        if names != ["laya_tell_me"]:
            raise RuntimeError(f"unexpected MCP tools: {names}")
        print("MCP discovery passed: laya_tell_me")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} /path/to/oh-my-laya-mcp")
    asyncio.run(verify(os.path.abspath(sys.argv[1])))
