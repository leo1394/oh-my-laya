"""Opt-in native DSH minimal-profile registration and tool discovery, no LLM."""
import json
import os
from pathlib import Path
import select
import shlex
import signal
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

from laya_tell_me.installer import register_dsh


DSH = os.environ.get("LAYA_DSH_TEST_BINARY")
BINARY = os.environ.get("LAYA_TEST_BINARY")


@unittest.skipUnless(DSH and BINARY, "Set explicit DSH and Laya test binaries")
class NativeDshRegistrationTests(unittest.TestCase):
    def test_native_profile_consumes_installer_patch_and_discovers_tools(self):
        for executable in (DSH, BINARY):
            self.assertTrue(Path(executable).is_file())
            self.assertTrue(os.access(executable, os.X_OK))
        with tempfile.TemporaryDirectory(prefix="laya-native-dsh-") as directory:
            root = Path(directory).resolve()
            dsh_home = root / "dsh"
            profile = dsh_home / "profiles" / "laya-native"
            profile.mkdir(parents=True)
            (profile / "package.json").write_text(json.dumps({
                "name": "laya-native-acceptance", "private": True,
                "dsh": {"profile": {"bundles": []}},
            }))
            (profile / "cordis.patch.yml").write_text(json.dumps([{"insert": [
                {"id": "system-prompt", "name": "@deepseek-ai/dsh-system-prompt"},
                {"id": "tools", "name": "@deepseek-ai/dsh-tools"},
                {"id": "laya-probe", "name": str(Path(__file__).with_name("dsh_native_tool_probe.mjs"))},
            ]}]))
            server = root / "oh-my-laya-mcp"
            server.write_text("#!/bin/sh\nexec " + shlex.quote(BINARY) + " mcp\n")
            server.chmod(0o700)
            workbench = root / "workbench"
            environment = {
                "HOME": str(root), "DSH_HOME": str(dsh_home),
                "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_CACHE_HOME": str(root / "cache"),
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "DSH_TELEMETRY_DISABLED": "1",
                "LAYA_WORKBENCH_DIR": str(workbench), "LAYA_PORT": "0",
                "LAYA_ADVISOR_CONFIG": str(root / "advisor.json"),
            }
            with patch.dict(os.environ, environment, clear=True):
                register_dsh(server, root / "unused-model", root, False)
            command = [DSH, "--profile", "laya-native"]
            composed = subprocess.run(command + ["--dump-config"], cwd=root, env=environment,
                capture_output=True, text=True, check=True, timeout=30)
            self.assertIn("@deepseek-ai/dsh-mcp-client", composed.stdout)
            self.assertIn(str(server), composed.stdout)
            # The profile intentionally has no LLM, filesystem or shell tool plugins.
            process = subprocess.Popen(command, cwd=root, env=environment, start_new_session=True,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            output = b""
            try:
                deadline = time.monotonic() + 30
                names = None
                while time.monotonic() < deadline:
                    if select.select([process.stdout], [], [], 0.1)[0]:
                        chunk = os.read(process.stdout.fileno(), 65536)
                        if not chunk:
                            break
                        output += chunk
                        for line in output.decode(errors="replace").splitlines():
                            if line.startswith("LAYA_NATIVE_TOOLS "):
                                names = json.loads(line.split(" ", 1)[1])
                    if names is not None:
                        break
                self.assertEqual(names, [
                    "mcp__oh-my-laya__laya_advisor_preferences",
                    "mcp__oh-my-laya__laya_feedback",
                    "mcp__oh-my-laya__laya_tell_me",
                ], output.decode(errors="replace"))
                self.assertFalse((workbench / "laya.sqlite3").exists())
                self.assertFalse((workbench / "service.sock").exists())
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
                finally:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.stdout.close()


if __name__ == "__main__":
    unittest.main()
