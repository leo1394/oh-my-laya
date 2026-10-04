"""Opt-in fresh bridge startup after atomic launcher replacement."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from laya_tell_me.runtime_install import install_binary, install_launchers


ROOT = Path(__file__).resolve().parents[1]
SOURCE = os.environ.get("LAYA_TEST_BINARY")


@unittest.skipUnless(SOURCE, "Set LAYA_TEST_BINARY to a verified release binary")
class BridgeUpgradeTests(unittest.TestCase):
    def test_fresh_bridge_launches_replacement_with_service_absent(self):
        with tempfile.TemporaryDirectory(prefix="laya-upgrade-", dir="/private/tmp") as directory:
            root = Path(directory)
            install = root / "install"
            commands = root / "bin"
            fixture = ROOT / "tests/fixtures/workbench_worker.py"
            previous = install_binary(install, Path(SOURCE))
            install_launchers(install, previous, fixture, root / "unused-model", bin_dir=commands)
            replacement = install_binary(install, Path(SOURCE))
            self.assertNotEqual(previous, replacement)
            install_launchers(install, replacement, fixture, root / "unused-model", bin_dir=commands)
            command = commands / "laya"
            env = {**os.environ, "LAYA_PORT": "0", "LAYA_ADVISOR_CONFIG": str(root / "advisor.json")}

            def status():
                return json.loads(subprocess.check_output([str(command), "status"], env=env, text=True, timeout=10))

            self.assertIs(status()["running"], False)
            request = {"jsonrpc": "2.0", "id": "cold-start", "method": "tools/call",
                       "params": {"name": "laya_advisor_preferences", "arguments": {"action": "get"}}}
            try:
                reply = subprocess.run([str(install / "bin/oh-my-laya-mcp")], env=env,
                                       input=json.dumps(request) + "\n", text=True, capture_output=True,
                                       check=True, timeout=30)
                result = json.loads(reply.stdout)["result"]
                self.assertFalse(result.get("isError"), result)
                state = status()
                executable = subprocess.check_output(["ps", "-p", str(state["service"]["pid"]), "-o", "command="], text=True)
                self.assertIn(str(replacement), executable)
                self.assertNotIn(str(previous), executable)
                self.assertFalse(state["settings"]["recording_enabled"])
                self.assertEqual(state["counts"]["decisions"], 0)
            finally:
                subprocess.run([str(command), "stop"], env=env, capture_output=True, check=True, timeout=10)
            self.assertIs(status()["running"], False)
