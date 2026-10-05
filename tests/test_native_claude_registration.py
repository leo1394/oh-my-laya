"""Opt-in native Claude registration/health check; no model requests or downloads."""
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import unittest
from unittest.mock import MagicMock, patch

from laya_tell_me.installer import register_claude


CLAUDE = os.environ.get("LAYA_CLAUDE_TEST_BINARY")
BINARY = os.environ.get("LAYA_TEST_BINARY")


def native_group(command, environment, root, check=True):
    process = subprocess.Popen(command, env=environment, cwd=root, start_new_session=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        stdout, stderr = process.communicate(timeout=30)
        result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
        if check:
            result.check_returncode()
        return result
    finally:
        # Include MCP descendants even when the CLI exits before them.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()


class NativeClaudeCleanupTests(unittest.TestCase):
    def test_timeout_kills_owned_group_and_reaps_parent(self):
        process = MagicMock(pid=12345)
        process.communicate.side_effect = subprocess.TimeoutExpired(["fixture-cli"], 30)
        with patch("subprocess.Popen", return_value=process) as spawn, patch("os.killpg") as kill:
            with self.assertRaises(subprocess.TimeoutExpired):
                native_group(["fixture-cli"], {}, Path("/private/tmp"))
        self.assertTrue(spawn.call_args.kwargs["start_new_session"])
        kill.assert_called_once_with(12345, signal.SIGKILL)
        process.wait.assert_called_once_with(timeout=5)
        process.stdout.close.assert_called_once()
        process.stderr.close.assert_called_once()


@unittest.skipUnless(CLAUDE and BINARY, "Set explicit Claude and Laya test binaries")
class NativeClaudeRegistrationTests(unittest.TestCase):
    def test_native_user_registration_health_and_removal_in_temporary_home(self):
        for executable in (CLAUDE, BINARY):
            self.assertTrue(Path(executable).is_file())
            self.assertTrue(os.access(executable, os.X_OK))
        with tempfile.TemporaryDirectory(prefix="laya-native-claude-") as directory:
            root = Path(directory).resolve()
            config = root / "claude"
            config.mkdir()
            workbench = root / "workbench"
            server = root / "oh-my-laya-mcp"
            server.write_text("#!/bin/sh\nexec " + shlex.quote(BINARY) + " mcp\n")
            server.chmod(0o700)
            environment = {
                "HOME": str(root), "CLAUDE_CONFIG_DIR": str(config),
                "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_CACHE_HOME": str(root / "cache"),
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "LAYA_WORKBENCH_DIR": str(workbench), "LAYA_PORT": "0",
                "LAYA_ADVISOR_CONFIG": str(root / "advisor.json"),
                "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1",
                "DISABLE_AUTOUPDATER": "1",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            }

            def native(command, *, check=True, **_kwargs):
                return native_group(command, environment, root, check)

            try:
                with patch.dict(os.environ, environment, clear=True), patch("laya_tell_me.installer.run", native):
                    register_claude(CLAUDE, server, root / "unused-model", False)
                stored = json.loads((config / ".claude.json").read_text())
                registration = stored["mcpServers"]["oh-my-laya"]
                self.assertEqual(registration["command"], str(server))
                self.assertEqual(registration["env"]["LAYA_MODEL_DIR"], str(root / "unused-model"))
                result = native([CLAUDE, "mcp", "get", "oh-my-laya"])
                self.assertIn("Connected", result.stdout)
                self.assertIn(str(server), result.stdout)
                listing = native([CLAUDE, "mcp", "list"])
                self.assertIn("oh-my-laya:", listing.stdout)
                self.assertIn("Connected", listing.stdout)
                # Initialize/tools-list must not start inference or persistence.
                self.assertFalse((workbench / "service.sock").exists())
                self.assertFalse((workbench / "laya.sqlite3").exists())
                print(native([CLAUDE, "--version"]).stdout.strip())
            finally:
                native([CLAUDE, "mcp", "remove", "oh-my-laya", "--scope", "user"], check=False)
                stored = json.loads((config / ".claude.json").read_text())
                self.assertNotIn("oh-my-laya", stored.get("mcpServers", {}))


if __name__ == "__main__":
    unittest.main()
