"""Opt-in native Codex and pi registration in isolated temporary homes."""
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import tempfile
import unittest
from io import StringIO
from unittest.mock import patch

from laya_tell_me import installer


ROOT = Path(__file__).resolve().parents[1]
CODEX_SOURCE = os.environ.get("LAYA_NATIVE_CODEX_BINARY")
PI_SOURCE = os.environ.get("LAYA_NATIVE_PI_BINARY")
LAYA_SOURCE = os.environ.get("LAYA_TEST_BINARY")
CODEX_BINARY = Path(CODEX_SOURCE) if CODEX_SOURCE else None
PI_BINARY = Path(PI_SOURCE) if PI_SOURCE else None
LAYA_BINARY = Path(LAYA_SOURCE) if LAYA_SOURCE else None


def isolated_environment(root):
    return {
        "HOME": str(root / "home"),
        "CODEX_HOME": str(root / "codex-home"),
        "PI_CODING_AGENT_DIR": str(root / "pi-agent"),
        "PI_CODING_AGENT_SESSION_DIR": str(root / "pi-sessions"),
        "PI_OFFLINE": "1",
        "LAYA_WORKBENCH_DIR": str(root / "workbench"),
        "TMPDIR": str(root / "tmp"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "LANG": "C.UTF-8",
    }


def run(command, environment, *, input_text=None, check=True, cwd=None):
    process = subprocess.Popen(
        [str(part) for part in command], env=environment, cwd=cwd,
        stdin=subprocess.PIPE if input_text is not None else subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        start_new_session=True,
    )
    timed_out = None
    stdout = ""
    stderr = ""
    try:
        stdout, stderr = process.communicate(input_text, timeout=30)
    except subprocess.TimeoutExpired as error:
        timed_out = error
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        if timed_out is not None:
            stdout, stderr = process.communicate()
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None:
                stream.close()
    if timed_out is not None:
        raise subprocess.TimeoutExpired(command, 30, output=stdout, stderr=stderr)
    completed = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    if check and completed.returncode:
        raise subprocess.CalledProcessError(
            completed.returncode, command, output=stdout, stderr=stderr
        )
    return completed


def mcp_wrapper(root):
    wrapper = root / "oh-my-laya-mcp"
    wrapper.write_text(
        "#!/bin/sh\nexec " + shlex.quote(str(LAYA_BINARY)) + " mcp \"$@\"\n"
    )
    wrapper.chmod(0o700)
    return wrapper


def installer_run(environment, default_cwd):
    def bounded(command, *, check=True, cwd=None, dry_run=False, **_options):
        if dry_run:
            return subprocess.CompletedProcess(command, 0, "", "")
        return run(command, environment, check=check, cwd=cwd or default_cwd)
    return bounded


@unittest.skipUnless(
    CODEX_SOURCE and PI_SOURCE and LAYA_SOURCE,
    "Set LAYA_NATIVE_CODEX_BINARY, LAYA_NATIVE_PI_BINARY and LAYA_TEST_BINARY",
)
class NativeHostRegistrationTests(unittest.TestCase):
    def setUp(self):
        for binary in (CODEX_BINARY, PI_BINARY, LAYA_BINARY):
            self.assertTrue(binary.is_file(), binary)
            self.assertTrue(os.access(binary, os.X_OK), binary)

    def test_codex_register_resolve_list_and_remove(self):
        with tempfile.TemporaryDirectory(prefix="laya-native-codex-", dir="/private/tmp") as directory:
            root = Path(directory)
            environment = isolated_environment(root)
            for name in ("home", "codex-home", "tmp", "cache", "config"):
                (root / name).mkdir()
            model_dir = root / "unused-model"
            model_dir.mkdir()
            selector = "oh-my-laya@personal"
            server = mcp_wrapper(root)
            bounded = installer_run(environment, root)

            with patch.dict(os.environ, environment, clear=True), \
                    patch.object(installer, "run", side_effect=bounded), \
                    patch("laya_tell_me.codex_plugin.subprocess.run", side_effect=bounded):
                installer.register_codex(str(CODEX_BINARY), server, model_dir, False)

            listed = run(
                [CODEX_BINARY, "plugin", "list", "--marketplace", "personal", "--json"],
                environment, cwd=root,
            )
            plugins = json.loads(listed.stdout)
            self.assertIn("oh-my-laya", json.dumps(plugins))
            resolved = run(
                [CODEX_BINARY, "mcp", "get", "oh-my-laya", "--json"],
                environment, cwd=root,
            )
            transport = json.loads(resolved.stdout)
            self.assertEqual(transport["transport"]["command"], str(server))
            self.assertEqual(
                transport["transport"]["env"]["LAYA_MODEL_DIR"], str(model_dir)
            )

            removed = run(
                [CODEX_BINARY, "plugin", "remove", selector, "--json"],
                environment, cwd=root,
            )
            removal = json.loads(removed.stdout)
            self.assertEqual(removal["pluginId"], selector)
            self.assertEqual(removal["marketplaceName"], "personal")
            after = json.loads(run(
                [CODEX_BINARY, "plugin", "list", "--marketplace", "personal", "--json"],
                environment, cwd=root,
            ).stdout)
            self.assertNotIn("oh-my-laya", json.dumps(after))
            self.assertFalse((root / "workbench").exists())

    def test_pi_register_list_load_without_prompt_and_remove(self):
        with tempfile.TemporaryDirectory(prefix="laya-native-pi-", dir="/private/tmp") as directory:
            root = Path(directory)
            environment = isolated_environment(root)
            for name in ("home", "pi-agent", "pi-sessions", "tmp", "cache", "config"):
                (root / name).mkdir()
            install_dir = root / "install"
            install_dir.mkdir()
            model_dir = root / "unused-model"
            model_dir.mkdir()
            server = mcp_wrapper(root)
            bounded = installer_run(environment, root)

            with patch.dict(os.environ, environment, clear=True), \
                    patch.object(installer, "run", side_effect=bounded):
                installer.register_pi(
                    str(PI_BINARY), ROOT, install_dir, server, model_dir, False
                )

            package = install_dir / "pi-package"
            listed = run([PI_BINARY, "list", "--no-approve"], environment, cwd=root)
            self.assertIn(str(package), listed.stdout)
            rpc = run(
                [
                    PI_BINARY, "--mode", "rpc", "--no-session", "--offline",
                    "--no-builtin-tools", "--tools", "laya_tell_me",
                ],
                environment,
                input_text=json.dumps({"id": "native-load", "type": "get_state"}) + "\n",
                cwd=root,
            )
            responses = [json.loads(line) for line in rpc.stdout.splitlines() if line.strip()]
            state = next(
                item for item in responses
                if item.get("type") == "response" and item.get("id") == "native-load"
            )
            self.assertTrue(state["success"], state)
            self.assertFalse((root / "workbench").exists())

            config = package / "laya.config.json"
            original_config = config.read_bytes()
            try:
                config.write_text("not-json\n")
                invalid = run(
                    [PI_BINARY, "--mode", "rpc", "--no-session", "--offline"],
                    environment,
                    input_text=json.dumps({"id": "negative", "type": "get_state"}) + "\n",
                    check=False,
                    cwd=root,
                )
                self.assertNotEqual(invalid.returncode, 0)
                self.assertIn("Failed to load extension", invalid.stderr)
            finally:
                config.write_bytes(original_config)

            removed = run(
                [PI_BINARY, "remove", str(package), "--no-approve"],
                environment, cwd=root,
            )
            self.assertEqual(removed.returncode, 0, removed.stderr)
            after = run([PI_BINARY, "list", "--no-approve"], environment, cwd=root)
            self.assertNotIn(str(package), after.stdout)
            settings = json.loads((root / "pi-agent" / "settings.json").read_text())
            self.assertNotIn(str(package), settings.get("packages", []))


class NativeProcessCleanupTests(unittest.TestCase):
    def test_process_group_is_reaped_on_success_error_and_timeout(self):
        class Process:
            next_pid = 9100

            def __init__(self, returncode, timeout=False):
                self.pid = Process.next_pid
                Process.next_pid += 1
                self.returncode = returncode
                self.timeout = timeout
                self.communications = 0
                self.waits = []
                self.kills = 0
                self.stdin = StringIO()
                self.stdout = StringIO()
                self.stderr = StringIO()

            def communicate(self, _input=None, timeout=None):
                self.communications += 1
                if self.timeout and self.communications == 1:
                    raise subprocess.TimeoutExpired("fixture", timeout)
                return "stdout", "stderr"

            def wait(self, timeout):
                self.waits.append(timeout)
                return self.returncode

            def kill(self):
                self.kills += 1

        cases = (("success", 0, False), ("error", 2, False), ("timeout", 0, True))
        for name, returncode, timeout in cases:
            with self.subTest(name=name):
                process = Process(returncode, timeout)
                groups = []
                with patch("subprocess.Popen", return_value=process), \
                        patch("os.killpg", side_effect=lambda pid, sig: groups.append((pid, sig))):
                    if name == "error":
                        with self.assertRaises(subprocess.CalledProcessError):
                            run(["fixture"], {}, cwd="/private/tmp")
                    elif name == "timeout":
                        with self.assertRaises(subprocess.TimeoutExpired):
                            run(["fixture"], {}, cwd="/private/tmp")
                    else:
                        self.assertEqual(run(["fixture"], {}, cwd="/private/tmp").returncode, 0)
                self.assertEqual(groups, [(process.pid, signal.SIGKILL)])
                self.assertEqual(process.waits, [5])
                self.assertEqual(process.kills, 0)
                self.assertTrue(process.stdin.closed)
                self.assertTrue(process.stdout.closed)
                self.assertTrue(process.stderr.closed)


if __name__ == "__main__":
    unittest.main()
