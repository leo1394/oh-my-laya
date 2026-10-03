import os
import shutil
from pathlib import Path
import subprocess
import tarfile
from tempfile import TemporaryDirectory
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "tools" / "oh-my-laya.sh"


class BootstrapTests(unittest.TestCase):
    def run_bootstrap(self, download_exit=0, install_exit=0, downloaders=("curl", "wget"), with_tar=True):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / "bin"
            binaries.mkdir()
            for name in ("tar", "bash", "mktemp", "rm", "mkdir", "cp", "gzip"):
                if name == "tar" and not with_tar:
                    continue
                (binaries / name).symlink_to(shutil.which(name))
            temporary = root / "temporary"
            temporary.mkdir()
            source = root / "fixture"
            source.mkdir()
            installer = source / "install.sh"
            installer.write_text(
                '#!/bin/bash\nprintf "arg=%s\\n" "$@"\n'
                f'exit {install_exit}\n'
            )
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                bundle.add(source, arcname="oh-my-laya-master")
            fixtures = {
                "uname": '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n',
            }
            for downloader in downloaders:
                if downloader == "git":
                    fixtures[downloader] = (
                        '#!/bin/sh\necho downloader=git\n'
                        f'exit_code={download_exit}\n'
                        '[ "$exit_code" -eq 0 ] || exit "$exit_code"\n'
                        'printf "git-arg=%s\\n" "$@"\n'
                        'for destination do :; done\n'
                        'cp -R "$LAYA_TEST_SOURCE" "$destination"\n'
                    )
                    continue
                flag = "--output" if downloader == "curl" else "-O"
                fixtures[downloader] = '#!/bin/sh\n' + (
                f'echo downloader={downloader}\n'
                f'exit_code={download_exit}\n'
                '[ "$exit_code" -eq 0 ] || exit "$exit_code"\n'
                'while [ "$#" -gt 0 ]; do\n'
                f'  if [ "$1" = {flag} ]; then cp "$LAYA_TEST_ARCHIVE" "$2"; exit; fi\n'
                '  shift\ndone\nexit 2\n'
                )
            for name, content in fixtures.items():
                executable = binaries / name
                executable.write_text(content)
                executable.chmod(0o755)
            environment = {
                **os.environ,
                "PATH": str(binaries),
                "TMPDIR": str(temporary),
                "LAYA_TEST_ARCHIVE": str(archive),
                "LAYA_TEST_SOURCE": str(source),
            }
            result = subprocess.run(
                ["/bin/sh", "-c", SCRIPT.read_text(), "oh-my-laya", "--dry-run"],
                env=environment, text=True, capture_output=True,
            )
            self.assertEqual(list(temporary.iterdir()), [])
            return result

    def test_sh_c_forwards_all_targets_and_arguments(self):
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("arg=--targets\narg=all\narg=--dry-run\n", result.stdout)
        self.assertIn("downloader=curl", result.stdout)
        self.assertNotIn("downloader=wget", result.stdout)

    def test_wget_fallback_without_curl(self):
        result = self.run_bootstrap(downloaders=("wget",))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("downloader=wget", result.stdout)
        self.assertIn("arg=--targets\narg=all\narg=--dry-run\n", result.stdout)

    def test_git_preferred_over_downloaders(self):
        result = self.run_bootstrap(downloaders=("git", "curl", "wget"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("downloader=git", result.stdout)
        self.assertNotIn("downloader=curl", result.stdout)
        self.assertNotIn("downloader=wget", result.stdout)
        self.assertIn("git-arg=clone\ngit-arg=--depth\ngit-arg=1\n", result.stdout)
        self.assertIn("git-arg=--branch\ngit-arg=master\n", result.stdout)
        self.assertIn("arg=--targets\narg=all\narg=--dry-run\n", result.stdout)

    def test_git_does_not_require_tar_or_downloaders(self):
        result = self.run_bootstrap(downloaders=("git",), with_tar=False)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_git_failure_does_not_fall_back(self):
        result = self.run_bootstrap(download_exit=128, downloaders=("git", "curl", "wget"))
        self.assertEqual(result.returncode, 128)
        self.assertNotIn("downloader=curl", result.stdout)
        self.assertNotIn("downloader=wget", result.stdout)
        self.assertNotIn("arg=--targets", result.stdout)

    def test_archive_requires_tar(self):
        result = self.run_bootstrap(with_tar=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("tar is required", result.stderr)

    def test_missing_downloaders(self):
        result = self.run_bootstrap(downloaders=())
        self.assertEqual(result.returncode, 1)
        self.assertIn("curl or wget is required", result.stderr)
        self.assertNotIn("arg=", result.stdout)

    def test_wget_failure_stops_installation(self):
        result = self.run_bootstrap(download_exit=8, downloaders=("wget",))
        self.assertEqual(result.returncode, 8)
        self.assertNotIn("arg=", result.stdout)

    def test_download_failure_stops_installation(self):
        result = self.run_bootstrap(download_exit=22)
        self.assertEqual(result.returncode, 22)
        self.assertNotIn("arg=", result.stdout)

    def test_installer_failure_is_propagated(self):
        result = self.run_bootstrap(install_exit=17)
        self.assertEqual(result.returncode, 17)


if __name__ == "__main__":
    unittest.main()
