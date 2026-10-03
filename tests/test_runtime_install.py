import io
import os
import subprocess
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
from pip._vendor.distlib.scripts import ScriptMaker, enquote_executable

from laya_tell_me import installer, runtime_install


def fake_binary(path: Path, version=runtime_install.WORKBENCH_VERSION):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "#!/bin/sh\n"
        f"if [ \"${{1:-}}\" = --version ]; then echo 'laya {version}'; exit 0; fi\n"
        "printf '%s\\n' \"$@\"\n"
        "printf 'python=%s\\nmodel=%s\\nworkbench=%s\\nsnake=%s\\n' "
        "\"$LAYA_PYTHON\" \"$LAYA_MODEL_DIR\" \"$LAYA_WORKBENCH_DIR\" \"$LAYA_SNAKE_BIN\"\n"
    )
    path.chmod(0o755)


def managed(path: Path, marker: str, body="exit 0\n"):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n" + marker + body)
    path.chmod(0o755)


class RuntimeInstallTests(unittest.TestCase):
    def test_immutable_binary_and_launchers_support_spaces(self):
        with TemporaryDirectory() as directory:
            root = Path(directory) / "runtime with spaces"
            source = Path(directory) / "build output" / "laya"
            fake_binary(source)
            binary = runtime_install.install_binary(root, source)
            python = root / "runtimes" / "python with spaces" / "bin" / "python"
            command, stdio = runtime_install.install_launchers(
                root, binary, python, root / "models" / "multi lingual",
                bin_dir=Path(directory) / "commands with spaces",
            )
            result = subprocess.run(
                [str(command), "status"], check=True, capture_output=True, text=True
            )
            self.assertIn(f"python={python}", result.stdout)
            self.assertIn(f"snake={python.parent / 'laya-snake'}", result.stdout)
            self.assertEqual(
                subprocess.run([str(stdio)], check=True, capture_output=True, text=True).stdout.splitlines()[0],
                "mcp",
            )
            self.assertTrue(binary.parent.name.startswith("workbench-0.2.0-"))

    def test_actual_venv_console_script_with_spaces_is_not_relocated(self):
        with TemporaryDirectory() as directory:
            runtime = Path(directory) / "immutable python runtime with spaces"
            python = installer._create_venv(runtime)
            version = subprocess.run(
                [str(python), "-c", "import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')"],
                check=True, capture_output=True, text=True,
            ).stdout.strip()
            site_packages = runtime / "lib" / f"python{version}" / "site-packages"
            (site_packages / "demo_entry.py").write_text(
                "def main():\n    print('entry point works')\n"
            )
            maker = ScriptMaker(None, str(python.parent))
            maker.executable = enquote_executable(str(python))
            maker.variants = {""}
            maker.make("laya-snake = demo_entry:main")
            script = python.parent / "laya-snake"
            script.chmod(0o755)
            result = subprocess.run([str(script)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr + script.read_text())
            self.assertEqual(result.stdout.strip(), "entry point works")
            self.assertIn(str(python), script.read_text())

    def test_legacy_python_stdio_path_is_preserved_and_backed_up(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "laya"
            fake_binary(source)
            binary = runtime_install.install_binary(root, source)
            legacy = root / "venv" / "bin" / "oh-my-laya-mcp"
            legacy.parent.mkdir(parents=True)
            old = ("#!/old/venv/bin/python\n"
                   "from laya_tell_me.server import main\nmain()\n")
            legacy.write_text(old)
            command = root / "commands" / "laya"
            old_command = ("#!/bin/sh\n" + runtime_install.LEGACY_LAUNCHER_MARKER
                           + "echo legacy snake\n")
            command.parent.mkdir()
            command.write_text(old_command)
            _, stdio = runtime_install.install_launchers(
                root, binary, root / "runtimes/python/bin/python",
                root / "models/multilingual", bin_dir=root / "commands",
            )
            self.assertEqual(stdio, legacy)
            self.assertEqual(
                (legacy.parent / "oh-my-laya-mcp.oh-my-laya-legacy-backup").read_text(), old
            )
            self.assertEqual(
                (command.parent / "laya.oh-my-laya-legacy-backup").read_text(), old_command
            )

    def test_default_prebuilt_is_actionable_until_release_is_pinned(self):
        with TemporaryDirectory() as directory, patch.object(runtime_install, "TRUSTED_RELEASES", {}):
            with self.assertRaisesRegex(RuntimeError, "--workbench-binary"):
                runtime_install.install_binary(Path(directory), None)

    def test_corrupt_download_leaves_active_install_untouched(self):
        class Response(io.BytesIO):
            def __enter__(self): return self
            def __exit__(self, *_): self.close()

        with TemporaryDirectory() as directory:
            root = Path(directory)
            old_binary = root / "runtimes/workbench-old/laya"
            fake_binary(old_binary)
            command = root / "commands/laya"
            stdio = root / "bin/oh-my-laya-mcp"
            managed(command, runtime_install.LAUNCHER_MARKER, f"exec {old_binary} \"$@\"\n")
            managed(stdio, runtime_install.STDIO_MARKER, f"exec {old_binary} mcp \"$@\"\n")
            before = command.read_bytes(), stdio.read_bytes(), old_binary.read_bytes()
            release = {runtime_install.WORKBENCH_VERSION: {
                "url": "https://example.invalid/laya", "sha256": "0" * 64,
            }}
            with patch.object(runtime_install, "TRUSTED_RELEASES", release):
                with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
                    runtime_install.install_binary(
                        root, None, urlopen=lambda *_args, **_kwargs: Response(b"corrupt")
                    )
            self.assertEqual(
                (command.read_bytes(), stdio.read_bytes(), old_binary.read_bytes()), before
            )
            self.assertEqual(
                [path for path in (root / "runtimes").iterdir()
                 if path.name.startswith("workbench-0.2.0-")], []
            )

    def test_second_launcher_failure_restores_both_old_wrappers(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            old_binary = root / "runtimes/workbench-old/laya"
            fake_binary(old_binary)
            command = root / "commands/laya"
            stdio = root / "bin/oh-my-laya-mcp"
            managed(command, runtime_install.LAUNCHER_MARKER, f"exec {old_binary} \"$@\"\n")
            managed(stdio, runtime_install.STDIO_MARKER, f"exec {old_binary} mcp \"$@\"\n")
            before = command.read_bytes(), stdio.read_bytes(), old_binary.read_bytes()
            source = root / "source/laya"
            fake_binary(source)
            binary = runtime_install.install_binary(root, source)
            real_replace = os.replace
            failed = False

            def fail_second(source_path, destination):
                nonlocal failed
                if Path(destination) == stdio and not failed:
                    failed = True
                    raise OSError("injected second launcher failure")
                return real_replace(source_path, destination)

            with patch.object(runtime_install.os, "replace", side_effect=fail_second):
                with self.assertRaisesRegex(OSError, "second launcher"):
                    runtime_install.install_launchers(
                        root, binary, root / "runtimes/python/bin/python",
                        root / "models/multilingual", bin_dir=root / "commands",
                    )
            self.assertEqual(
                (command.read_bytes(), stdio.read_bytes(), old_binary.read_bytes()), before
            )
            self.assertTrue(binary.is_file())

    def test_python_failure_removes_new_binary_without_touching_active_wrappers(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            old_binary = root / "runtimes/workbench-old/laya"
            fake_binary(old_binary)
            command = root / "commands/laya"
            stdio = root / "bin/oh-my-laya-mcp"
            managed(command, runtime_install.LAUNCHER_MARKER, f"exec {old_binary} \"$@\"\n")
            managed(stdio, runtime_install.STDIO_MARKER, f"exec {old_binary} mcp \"$@\"\n")
            before = command.read_bytes(), stdio.read_bytes(), old_binary.read_bytes()
            source = root / "source/laya"
            fake_binary(source)
            created = None

            def remember_binary(*args, **kwargs):
                nonlocal created
                created = runtime_install.install_binary(root, source)
                return created

            with patch.object(installer, "install_binary", side_effect=remember_binary), \
                    patch.object(installer, "_install_python_runtime", side_effect=RuntimeError("pip failed")):
                with self.assertRaisesRegex(RuntimeError, "pip failed"):
                    installer.install_runtime(Path(directory), root, "multilingual", False, source)
            self.assertFalse(created.parent.exists())
            self.assertEqual(
                (command.read_bytes(), stdio.read_bytes(), old_binary.read_bytes()), before
            )

    def test_unmanaged_launcher_is_never_overwritten(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "commands/laya"
            target.parent.mkdir()
            target.write_text("#!/bin/sh\necho custom\n")
            with self.assertRaisesRegex(RuntimeError, "unmanaged"):
                runtime_install.validate_launcher_ownership(root, bin_dir=target.parent)

    def test_wrong_local_version_is_rejected_and_cleaned_up(self):
        with TemporaryDirectory() as directory:
            root = Path(directory) / "runtime"
            source = Path(directory) / "wrong"
            fake_binary(source, "9.9.9")
            with self.assertRaisesRegex(RuntimeError, "Unexpected.*version"):
                runtime_install.install_binary(root, source)
            self.assertEqual(list((root / "runtimes").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
