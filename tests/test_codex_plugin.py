import json
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import Mock, patch

from laya_tell_me.codex_plugin import install_codex_plugin


class CodexPluginTests(unittest.TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.home = Path(self.directory.name)
        environment = patch.dict("os.environ", {"CODEX_HOME": str(self.home / ".codex")})
        environment.start()
        self.addCleanup(environment.stop)
        home = patch("laya_tell_me.codex_plugin.Path.home", return_value=self.home)
        home.start()
        self.addCleanup(home.stop)
        process = patch("laya_tell_me.codex_plugin.subprocess.run",
                        return_value=subprocess.CompletedProcess([], 1, "", ""))
        self.process = process.start()
        self.addCleanup(process.stop)
        self.run = Mock()
        self.plugin = self.home / "plugins/oh-my-laya"
        self.catalog = self.home / ".agents/plugins/marketplace.json"

    def install(self, dry_run=False):
        install_codex_plugin("codex", Path("/custom/bin/server"),
                             Path("/custom/models"), dry_run, self.run)

    def test_dry_run_writes_nothing(self):
        self.install(True)
        self.assertEqual(list(self.home.iterdir()), [])
        self.process.assert_not_called()

    def test_install_and_repeat_preserve_catalog_and_paths(self):
        self.catalog.parent.mkdir(parents=True)
        other = {"name": "other"}
        self.catalog.write_text(json.dumps({"name": "mine", "interface": {"displayName": "Mine"},
                                            "plugins": [other]}))
        self.install()
        self.install()
        catalog = json.loads(self.catalog.read_text())
        self.assertEqual(catalog["interface"]["displayName"], "Mine")
        self.assertEqual(catalog["plugins"][0], other)
        self.assertEqual(len(catalog["plugins"]), 2)
        mcp = json.loads((self.plugin / ".mcp.json").read_text())["mcpServers"]["oh-my-laya"]
        self.assertEqual(mcp["command"], "/custom/bin/server")
        self.assertEqual(mcp["env"]["LAYA_MODEL_DIR"], "/custom/models")
        self.run.assert_called_with(["codex", "plugin", "add", "oh-my-laya@mine"])

    def test_failure_restores_previous_files(self):
        self.install()
        before = (self.plugin / ".codex-plugin/plugin.json").read_bytes()
        catalog = self.catalog.read_bytes()
        self.run.side_effect = RuntimeError("install failed")
        with self.assertRaisesRegex(RuntimeError, "install failed"):
            self.install()
        self.assertEqual((self.plugin / ".codex-plugin/plugin.json").read_bytes(), before)
        self.assertEqual(self.catalog.read_bytes(), catalog)
        self.assertFalse(any(call.args[0][1:3] == ["mcp", "remove"] for call in self.run.call_args_list))

    def test_protect_modified_plugin(self):
        self.install()
        (self.plugin / ".mcp.json").write_text("{}")
        with self.assertRaisesRegex(RuntimeError, "locally modified"):
            self.install()

    def test_first_install_failure_leaves_no_plugin_or_catalog(self):
        self.run.side_effect = RuntimeError("install failed")
        with self.assertRaisesRegex(RuntimeError, "install failed"):
            self.install()
        self.assertFalse(self.plugin.exists())
        self.assertFalse(self.catalog.exists())

    def test_unrelated_legacy_registration_is_preserved(self):
        config = self.home / ".codex/config.toml"
        config.parent.mkdir()
        config.write_text('[mcp_servers.oh-my-laya]\ncommand = "/other/server"\n')
        self.install()
        self.run.assert_called_once_with(["codex", "plugin", "add", "oh-my-laya@personal"])

    def test_remove_only_matching_legacy_after_success(self):
        config = self.home / ".codex/config.toml"
        config.parent.mkdir()
        config.write_text('[mcp_servers.oh-my-laya]\ncommand = "/custom/bin/server"\n'
                          '[mcp_servers.oh-my-laya.env]\nLAYA_MODEL_DIR = "/custom/models"\n')
        self.install()
        self.assertEqual(self.run.call_args_list[0].args[0][1:3], ["plugin", "add"])
        self.run.assert_called_with(["codex", "mcp", "remove", "oh-my-laya"])

    def test_conflicting_catalog_is_preserved(self):
        self.catalog.parent.mkdir(parents=True)
        self.catalog.write_text(json.dumps({"name": "personal", "plugins": [{"name": "oh-my-laya"}]}))
        before = self.catalog.read_bytes()
        with self.assertRaisesRegex(RuntimeError, "Conflicting"):
            self.install()
        self.assertEqual(self.catalog.read_bytes(), before)

    def test_symlink_parent_is_rejected(self):
        (self.home / "plugins").symlink_to(self.home)
        with self.assertRaisesRegex(RuntimeError, "symlinked"):
            self.install()
