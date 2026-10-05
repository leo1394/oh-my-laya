import io
from pathlib import Path
import shutil
import subprocess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
import zipfile

from laya_tell_me.installer import (
    _download_alpha_squad_skill,
    _safe_alpha_archive_extract,
    _local_alpha_squad_skill,
    ALPHA_SQUAD_PATH,
    ALPHA_SQUAD_REVISION,
    register_alpha_squad_skill,
)


class AlphaSquadInstallTests(unittest.TestCase):
    def make_fixture(self, directory):
        source = Path(directory) / "fixture" / "alpha-squad-coding-craft"
        (source / "assets" / "codex-agents").mkdir(parents=True)
        (source / "SKILL.md").write_text("# Alpha Squad\n")
        (source / "agents" / "openai.yaml").parent.mkdir(exist_ok=True)
        (source / "agents" / "openai.yaml").write_text("roles: []\n")
        (source / "assets" / "codex-agents" / "worker.toml").write_text("role = 'worker'\n")
        return source

    def install(self, source, codex, agents):
        with patch("laya_tell_me.installer._local_alpha_squad_skill", return_value=source):
            return register_alpha_squad_skill(False, codex, agents)

    def make_archive(self, directory, entries):
        archive = Path(directory) / "alpha.zip"
        with zipfile.ZipFile(archive, "w") as bundle:
            for name, content, attributes in entries:
                info = zipfile.ZipInfo(name)
                info.external_attr = attributes
                bundle.writestr(info, content)
        return archive

    def test_dry_run_does_not_fetch_or_write(self):
        with TemporaryDirectory() as directory:
            codex = Path(directory) / "codex-skills"
            agents = Path(directory) / "agents-skills"
            with patch("laya_tell_me.installer._download_alpha_squad_skill") as fetch:
                self.assertTrue(register_alpha_squad_skill(True, codex, agents))
            fetch.assert_not_called()
            self.assertFalse((codex / "alpha-squad-coding-craft").exists())

    def test_installs_full_tree_and_idempotently_updates_managed_skill(self):
        with TemporaryDirectory() as directory:
            source = self.make_fixture(directory)
            codex = Path(directory) / "codex-skills"
            agents = Path(directory) / "agents-skills"
            target = codex / "alpha-squad-coding-craft"

            self.assertTrue(self.install(source, codex, agents))
            self.assertEqual((target / "SKILL.md").read_text(), "# Alpha Squad\n")
            self.assertEqual(
                (target / "assets" / "codex-agents" / "worker.toml").read_text(),
                "role = 'worker'\n",
            )
            (source / "SKILL.md").write_text("# Latest Alpha Squad\n")
            self.assertTrue(self.install(source, codex, agents))
            self.assertEqual((target / "SKILL.md").read_text(), "# Latest Alpha Squad\n")

    def test_local_and_unmanaged_skills_are_protected_before_fetch(self):
        with TemporaryDirectory() as directory:
            source = self.make_fixture(directory)
            codex = Path(directory) / "codex-skills"
            agents = Path(directory) / "agents-skills"
            target = codex / "alpha-squad-coding-craft"
            self.install(source, codex, agents)
            (target / "SKILL.md").write_text("local edit")
            with patch("laya_tell_me.installer._download_alpha_squad_skill") as fetch:
                self.assertFalse(register_alpha_squad_skill(False, codex, agents))
            fetch.assert_not_called()
            self.assertEqual((target / "SKILL.md").read_text(), "local edit")

            shutil.rmtree(target)
            target.mkdir(parents=True)
            (target / "SKILL.md").write_text("unmanaged")
            with patch("laya_tell_me.installer._download_alpha_squad_skill") as fetch:
                self.assertFalse(register_alpha_squad_skill(False, codex, agents))
            fetch.assert_not_called()
            self.assertEqual((target / "SKILL.md").read_text(), "unmanaged")

            shutil.rmtree(target)
            linked = Path(directory) / "linked-skill"
            linked.mkdir()
            target.symlink_to(linked, target_is_directory=True)
            with patch("laya_tell_me.installer._download_alpha_squad_skill") as fetch:
                self.assertFalse(register_alpha_squad_skill(False, codex, agents))
            fetch.assert_not_called()
            self.assertTrue(target.is_symlink())

    def test_existing_managed_agents_location_is_updated(self):
        with TemporaryDirectory() as directory:
            source = self.make_fixture(directory)
            codex = Path(directory) / "codex-skills"
            agents = Path(directory) / "agents-skills"
            self.assertTrue(self.install(source, agents, codex))

            (source / "SKILL.md").write_text("# Updated\n")
            self.assertTrue(self.install(source, codex, agents))
            self.assertFalse((codex / "alpha-squad-coding-craft").exists())
            self.assertEqual(
                (agents / "alpha-squad-coding-craft" / "SKILL.md").read_text(),
                "# Updated\n",
            )

    def test_duplicate_existing_locations_are_ambiguous_before_fetch(self):
        with TemporaryDirectory() as directory:
            source = self.make_fixture(directory)
            codex = Path(directory) / "codex-skills"
            agents = Path(directory) / "agents-skills"
            self.install(source, codex, agents)
            other = agents / "alpha-squad-coding-craft"
            other.mkdir(parents=True)
            (other / "SKILL.md").write_text("other")
            with patch("laya_tell_me.installer._download_alpha_squad_skill") as fetch:
                self.assertFalse(register_alpha_squad_skill(False, codex, agents))
            fetch.assert_not_called()

    def test_git_clone_uses_default_branch_and_does_not_fallback(self):
        with TemporaryDirectory() as directory:
            workspace = Path(directory)

            def clone(command, **_):
                checkout = Path(command[-1])
                (checkout / "skills" / "alpha-squad-coding-craft").mkdir(parents=True)
                (checkout / "skills" / "alpha-squad-coding-craft" / "SKILL.md").write_text("ok")
                return subprocess.CompletedProcess(command, 0, "", "")

            with patch("laya_tell_me.installer.shutil.which", return_value="git"), \
                    patch("laya_tell_me.installer.subprocess.run", side_effect=clone) as run:
                source = _download_alpha_squad_skill(workspace)
            command = run.call_args.args[0]
            self.assertIn("--depth", command)
            self.assertIn("1", command)
            self.assertNotIn("--branch", command)
            self.assertTrue((source / "SKILL.md").is_file())

            failure = subprocess.CalledProcessError(1, ["git"], stderr="offline")
            with patch("laya_tell_me.installer.shutil.which", return_value="git"), \
                    patch("laya_tell_me.installer.subprocess.run", side_effect=failure), \
                    patch("laya_tell_me.installer.request.urlopen") as download:
                with self.assertRaisesRegex(RuntimeError, "Unable to clone"):
                    _download_alpha_squad_skill(workspace)
            download.assert_not_called()

    def test_archive_extraction_accepts_only_a_safe_skill_subtree(self):
        with TemporaryDirectory() as directory:
            safe = self.make_archive(directory, [
                ("repo/skills/alpha-squad-coding-craft/SKILL.md", "ok", 0),
                ("repo/skills/alpha-squad-coding-craft/agents/role.toml", "role", 0),
            ])
            source = _safe_alpha_archive_extract(safe, Path(directory) / "safe")
            self.assertEqual((source / "SKILL.md").read_text(), "ok")

            with patch("laya_tell_me.installer.shutil.which", return_value=None), \
                    patch("laya_tell_me.installer.request.urlopen", return_value=io.BytesIO(safe.read_bytes())):
                downloaded = _download_alpha_squad_skill(Path(directory) / "download")
            self.assertEqual((downloaded / "SKILL.md").read_text(), "ok")

            traversal = self.make_archive(directory, [
                ("repo/../outside", "bad", 0),
            ])
            with self.assertRaisesRegex(RuntimeError, "unsafe path"):
                _safe_alpha_archive_extract(traversal, Path(directory) / "traversal")

            symlink = self.make_archive(directory, [
                ("repo/skills/alpha-squad-coding-craft/link", "target", 0o120777 << 16),
            ])
            with self.assertRaisesRegex(RuntimeError, "symlink"):
                _safe_alpha_archive_extract(symlink, Path(directory) / "symlink")

            missing = self.make_archive(directory, [
                ("repo/README.md", "none", 0),
            ])
            with self.assertRaisesRegex(RuntimeError, "missing"):
                _safe_alpha_archive_extract(missing, Path(directory) / "missing")

            with patch("laya_tell_me.installer.shutil.which", return_value=None), \
                    patch("laya_tell_me.installer.request.urlopen", return_value=io.BytesIO(b"not a zip")):
                with self.assertRaisesRegex(RuntimeError, "invalid"):
                    _download_alpha_squad_skill(Path(directory) / "invalid")

            oversized = self.make_archive(directory, [
                ("repo/skills/alpha-squad-coding-craft/SKILL.md", "xxxxx", 0),
            ])
            with patch("laya_tell_me.installer.MAX_ALPHA_EXTRACTED_BYTES", 4), \
                    self.assertRaisesRegex(RuntimeError, "exceeds"):
                _safe_alpha_archive_extract(oversized, Path(directory) / "oversized")

    def test_download_failure_and_concurrent_edit_preserve_existing_skill(self):
        with TemporaryDirectory() as directory:
            source = self.make_fixture(directory)
            codex = Path(directory) / "codex-skills"
            agents = Path(directory) / "agents-skills"
            target = codex / "alpha-squad-coding-craft"
            self.install(source, codex, agents)
            before = (target / "SKILL.md").read_text()

            with patch("laya_tell_me.installer._local_alpha_squad_skill", side_effect=RuntimeError("offline")):
                with self.assertRaisesRegex(RuntimeError, "offline"):
                    register_alpha_squad_skill(False, codex, agents)
            self.assertEqual((target / "SKILL.md").read_text(), before)

            (source / "SKILL.md").write_text("new upstream")

            def concurrent_edit(_):
                (target / "SKILL.md").write_text("user edit")
                return source

            with patch("laya_tell_me.installer._local_alpha_squad_skill", side_effect=concurrent_edit):
                with self.assertRaisesRegex(RuntimeError, "changed during download"):
                    register_alpha_squad_skill(False, codex, agents)
            self.assertEqual((target / "SKILL.md").read_text(), "user edit")

    def test_local_submodule_is_used_offline_without_git_or_download(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / ALPHA_SQUAD_PATH / ALPHA_SQUAD_PATH
            source.mkdir(parents=True)
            (source / "SKILL.md").write_text("local edit")
            with patch("laya_tell_me.installer.subprocess.run") as run, \
                    patch("laya_tell_me.installer.request.urlopen") as download:
                self.assertEqual(_local_alpha_squad_skill(root), source)
                self.assertTrue(register_alpha_squad_skill(
                    False, root / "codex", root / "agents", source_root=root,
                ))
            run.assert_not_called()
            download.assert_not_called()
            self.assertEqual((root / "codex/alpha-squad-coding-craft/SKILL.md").read_text(), "local edit")

    def test_uninitialized_submodule_uses_https_and_gitlink(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".git").write_text("gitdir: elsewhere")
            source = root / ALPHA_SQUAD_PATH / ALPHA_SQUAD_PATH
            def initialize(command, **kwargs):
                self.assertEqual(kwargs["cwd"], root)
                self.assertIn("submodule.skills/alpha-squad-coding-craft.url=https://github.com/leo1394/skill-alpha-squad-coding-craft.git", command)
                self.assertEqual(command[-5:], ["submodule", "update", "--init", "--", ALPHA_SQUAD_PATH.as_posix()])
                source.mkdir(parents=True)
                (source / "SKILL.md").write_text("initialized")
            with patch("laya_tell_me.installer.shutil.which", return_value="git"), \
                    patch("laya_tell_me.installer.subprocess.run", side_effect=initialize), \
                    patch("laya_tell_me.installer.request.urlopen") as download:
                self.assertEqual(_local_alpha_squad_skill(root), source)
            download.assert_not_called()

    def test_archive_source_populates_local_path_at_pinned_revision(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            archive = self.make_archive(directory, [
                ("repo/skills/alpha-squad-coding-craft/SKILL.md", "pinned", 0),
            ])
            with patch("laya_tell_me.installer.shutil.which", return_value=None), \
                    patch("laya_tell_me.installer.request.urlopen", return_value=io.BytesIO(archive.read_bytes())) as download:
                source = _local_alpha_squad_skill(root)
            self.assertEqual(source, root / ALPHA_SQUAD_PATH / ALPHA_SQUAD_PATH)
            self.assertEqual((source / "SKILL.md").read_text(), "pinned")
            self.assertTrue(download.call_args.args[0].endswith("/" + ALPHA_SQUAD_REVISION))

    def test_incomplete_or_symlinked_local_checkout_is_not_overwritten(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            checkout = root / ALPHA_SQUAD_PATH
            checkout.mkdir(parents=True)
            (checkout / "keep").write_text("local")
            with self.assertRaisesRegex(RuntimeError, "Incomplete"):
                _local_alpha_squad_skill(root)
            self.assertEqual((checkout / "keep").read_text(), "local")
            other = root / "linked"
            (other / "skills").mkdir(parents=True)
            (other / ALPHA_SQUAD_PATH).symlink_to(checkout, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "symlinked"):
                _local_alpha_squad_skill(other)

    def test_archive_revision_matches_submodule_gitlink(self):
        root = Path(__file__).resolve().parents[1]
        if not (root / ".git").exists() or not shutil.which("git"):
            self.skipTest("source archive has no gitlink metadata")
        entry = subprocess.check_output(
            ["git", "ls-files", "--stage", ALPHA_SQUAD_PATH.as_posix()], cwd=root, text=True,
        ).split()
        self.assertEqual(entry[:2], ["160000", ALPHA_SQUAD_REVISION])


if __name__ == "__main__":
    unittest.main()
