"""Opt-in real MCP + MLX check; uses isolated preferences and a fixture catalog.

LAYA_MODEL_DIR=/path/to/model PYTHONPATH=src python tests/smoke_advisor.py
"""

import asyncio
import json
import os
from pathlib import Path
import sys
from tempfile import TemporaryDirectory

from mcp import Client, StdioServerParameters


async def main():
    with TemporaryDirectory() as directory:
        environment = dict(os.environ)
        environment["LAYA_ADVISOR_CONFIG"] = str(Path(directory) / "advisor.json")
        async with Client(StdioServerParameters(
            command=sys.executable,
            args=["-m", "laya_tell_me.server"],
            env=environment,
        )) as client:
            tools = await client.list_tools()
            assert {"laya_tell_me", "laya_advisor_preferences"} <= {
                tool.name for tool in tools.tools
            }
            settings = await client.call_tool("laya_advisor_preferences", {})
            assert settings.structured_content["needs_policy_selection"]
            saved = await client.call_tool("laya_advisor_preferences", {"policy": "conditional"})
            assert saved.structured_content["policy"] == "conditional"
            response = await client.call_tool("laya_tell_me", {
                "state": "Fix a spelling mistake in README only. No code or runtime behavior changes.",
                "advisor": {
                    "models": [{"id": "fixture-model", "reasoning_efforts": ["low", "medium", "high"]}],
                    "current_model": "fixture-model",
                },
            })
            assert not response.is_error, response
            payload = response.structured_content
            assert not payload["advice"]["model_switched"]
            assert payload["advice"]["policy"] == "conditional"
            assert payload["laya_result"]["answers"]
            print(json.dumps(payload, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    asyncio.run(main())
