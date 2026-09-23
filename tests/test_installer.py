from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from laya_agent_bridge.installer import (
    MANAGED_END,
    MANAGED_START,
    dsh_block,
    parse_targets,
    replace_managed_block,
    resolve_executable,
)


class InstallerTests(unittest.TestCase):
    def test_resolve_executable_uses_app_bundle_fallback(self):
        with TemporaryDirectory() as directory:
            executable = Path(directory) / "codex"
            executable.write_text("#!/bin/sh\n")
            executable.chmod(0o755)

            resolved = resolve_executable(
                "codex",
                which=lambda _: None,
                fallback_paths=(executable,),
            )

        self.assertEqual(resolved, str(executable))

    def test_parse_all_detected_targets(self):
        self.assertEqual(parse_targets("all", {"codex", "pi"}), ["codex", "pi"])
        self.assertEqual(
            parse_targets("both", {"claude", "codex"}), ["claude", "codex"]
        )

    def test_parse_multiple_targets_and_aliases(self):
        self.assertEqual(
            parse_targets("codex,deepseek,pi-agent", {"codex", "dsh", "pi"}),
            ["codex", "dsh", "pi"],
        )

    def test_parse_rejects_missing_client(self):
        with self.assertRaisesRegex(ValueError, "not installed"):
            parse_targets("claude", {"codex"})

    def test_managed_block_is_idempotently_replaced(self):
        original = f"before\n{MANAGED_START}\nold\n{MANAGED_END}\nafter\n"
        updated = replace_managed_block(
            original, f"{MANAGED_START}\nnew\n{MANAGED_END}"
        )
        self.assertEqual(updated.count(MANAGED_START), 1)
        self.assertNotIn("old", updated)
        self.assertIn("before", updated)
        self.assertIn("after", updated)

    def test_dsh_block_contains_absolute_runtime_paths(self):
        block = dsh_block(
            Path("/opt/laya/bin/oh-my-laya-mcp"),
            Path("/opt/laya/models/multilingual"),
            Path("/opt/laya"),
        )
        self.assertIn("@deepseek-ai/dsh-mcp-client", block)
        self.assertIn("command: '/opt/laya/bin/oh-my-laya-mcp'", block)
        self.assertIn(
            "LAYA_MODEL_DIR: '/opt/laya/models/multilingual'", block
        )


if __name__ == "__main__":
    unittest.main()
