"""Exercise release path selection without building or downloading dependencies."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class ReleasePackagingTests(unittest.TestCase):
    def package(self, target=None, fail=False):
        with tempfile.TemporaryDirectory(prefix="laya-release-") as directory:
            root = Path(directory)
            (root / "tools").mkdir()
            shutil.copy2(ROOT / "tools/package-workbench-release.sh", root / "tools/package.sh")
            (root / "tools/version.py").write_text("print('2.0.1')\n")
            (root / "web/dist").mkdir(parents=True)
            (root / "web/dist/index.html").write_text("fixture")
            stale = root / "target/release/laya"
            stale.parent.mkdir(parents=True)
            stale.write_text("stale")
            stale.chmod(0o755)
            commands = root / "commands"
            commands.mkdir()
            for name, script in {
                "uname": 'case "$1" in -s) echo Darwin;; -m) echo arm64;; esac',
                "npm": "exit 0",
                "cargo": '''[ "${FAIL_BUILD:-}" != 1 ] || exit 9
while [ "$#" -gt 0 ]; do
  if [ "$1" = --target-dir ]; then shift; build_dir=$1; fi
  shift
done
mkdir -p "$build_dir/release"
printf fresh > "$build_dir/release/laya"
chmod 755 "$build_dir/release/laya"''',
            }.items():
                command = commands / name
                command.write_text("#!/bin/sh\nset -eu\n" + script + "\n")
                command.chmod(0o755)
            env = {**os.environ, "PATH": str(commands) + os.pathsep + os.environ["PATH"]}
            env.pop("CARGO_TARGET_DIR", None)
            if target is not None:
                env["CARGO_TARGET_DIR"] = str(root / "absolute target") if target == "absolute" else target
            if fail:
                env["FAIL_BUILD"] = "1"
            output = root / "release output"
            result = subprocess.run(["sh", str(root / "tools/package.sh"), str(output)],
                                    env=env, capture_output=True, text=True)
            if fail:
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((output / "laya-macos-arm64").exists())
            else:
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((output / "laya-macos-arm64").read_text(), "fresh")
                checked = subprocess.run(["shasum", "-a", "256", "-c", "laya-macos-arm64.sha256"],
                                         cwd=output, capture_output=True, text=True)
                self.assertEqual(checked.returncode, 0, checked.stderr)

    def test_default_target(self):
        self.package()

    def test_absolute_target_with_spaces(self):
        self.package("absolute")

    def test_relative_target_with_spaces(self):
        self.package("custom target")

    def test_failed_build_never_packages_stale_binary(self):
        self.package(fail=True)
