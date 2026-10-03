from pathlib import Path
from tempfile import TemporaryDirectory
import subprocess
import tomllib
import unittest

from laya_tell_me.installer import install_launcher


class LauncherTests(unittest.TestCase):
    def test_demo_dependency(self):
        project = Path(__file__).resolve().parents[1] / "pyproject.toml"
        self.assertIn("laya-mlx[demo]==0.2.0", tomllib.loads(project.read_text())["project"]["dependencies"])

    def test_launch_and_reinstall_with_custom_paths(self):
        with TemporaryDirectory() as directory:
            root = Path(directory) / "custom space's install"
            bin_dir = Path(directory) / "bin"
            snake = root / "venv" / "bin" / "laya-snake"
            snake.parent.mkdir(parents=True)
            snake.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            snake.chmod(0o755)
            for model in ("multilingual", "english"):
                install_launcher(root, model, False, bin_dir)
                result = subprocess.run([str(bin_dir / "laya"), "--snake", "--steps", "1"], capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout.splitlines(), ["--model", str(root / "models" / model), "--steps", "1"])
            self.assertEqual(subprocess.run([str(bin_dir / "laya"), "--bad"], capture_output=True).returncode, 2)
            self.assertEqual(subprocess.run([str(bin_dir / "laya"), "--help"], capture_output=True).returncode, 0)

    def test_dry_run_and_conflicts(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir = root / "bin"
            install_launcher(root, "multilingual", True, bin_dir)
            self.assertFalse(bin_dir.exists())
            bin_dir.mkdir()
            target = bin_dir / "laya"
            target.write_text("user command")
            with self.assertRaises(RuntimeError):
                install_launcher(root, "multilingual", False, bin_dir)
            self.assertEqual(target.read_text(), "user command")
            target.unlink()
            target.symlink_to(root / "missing")
            with self.assertRaises(RuntimeError):
                install_launcher(root, "multilingual", False, bin_dir)
