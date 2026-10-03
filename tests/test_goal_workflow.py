from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from laya_tell_me.goal_workflow import (
    TITLE, codex_home, install_goal_workflow, replace_goal_section,
    render_rules, select_goal_workflow,
)
from laya_tell_me import installer


class GoalWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve() / "custom codex"
        self.agents = Path(self.temp.name).resolve() / "agents" / "skills"

    def install(self, dry_run=False):
        install_goal_workflow(dry_run, home=self.home, agents_skills=self.agents)

    def skill(self, root, name):
        path = root / name / "SKILL.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("test skill\n")
        return path

    def prepare(self):
        alpha = self.skill(self.home / "skills", "alpha-squad-coding-craft")
        advisor = self.skill(self.agents, "laya-model-advisor")
        return alpha, advisor

    def test_prompt_accept_decline_and_invalid(self):
        with patch("sys.stdin.isatty", return_value=True), patch("builtins.input", side_effect=["wrong", "Y"]):
            self.assertTrue(select_goal_workflow(None))
        for answer in ("", "n", "no"):
            with patch("sys.stdin.isatty", return_value=True), patch("builtins.input", return_value=answer):
                self.assertFalse(select_goal_workflow(None))

    def test_noninteractive_dry_run_and_explicit_flags(self):
        with patch("sys.stdin.isatty", return_value=False), patch("builtins.input") as prompt:
            self.assertFalse(select_goal_workflow(None))
            self.assertFalse(select_goal_workflow(None, True))
            self.assertTrue(select_goal_workflow("yes"))
            self.assertFalse(select_goal_workflow("no"))
            prompt.assert_not_called()

    def test_fresh_write_uses_actual_paths_and_is_idempotent(self):
        alpha, advisor = self.prepare()
        self.install()
        path = self.home / "AGENTS.md"
        first = path.read_text()
        self.assertIn(str(alpha), first)
        self.assertIn(str(advisor), first)
        self.assertNotIn("yuyl_field", first)
        self.assertIn("SAME window", first)
        self.install()
        self.assertEqual(path.read_text(), first)
        self.assertEqual(list(self.home.glob("AGENTS.md.oh-my-laya-backup-*")), [])

    def test_existing_section_backup_and_unrelated_content_preserved(self):
        self.prepare()
        original = f"## Tone\nKeep exactly.\n\n{TITLE}\nold path\n\n## Other\nkeep tail\n"
        path = self.home / "AGENTS.md"
        path.write_text(original)
        path.chmod(0o640)
        self.install()
        content = path.read_text()
        self.assertTrue(content.startswith("## Tone\nKeep exactly.\n\n"))
        self.assertTrue(content.endswith("## Other\nkeep tail\n"))
        self.assertEqual(content.count(TITLE), 1)
        self.assertNotIn("old path", content)
        self.assertEqual(path.stat().st_mode & 0o777, 0o640)
        backup = list(self.home.glob("AGENTS.md.oh-my-laya-backup-*"))
        self.assertEqual(len(backup), 1)
        self.assertEqual(backup[0].read_text(), original)

    def test_dry_run_creates_nothing_even_with_missing_skills(self):
        self.install(True)
        self.assertFalse(self.home.exists())

    def test_agents_skill_location_and_custom_codex_home(self):
        alpha = self.skill(self.agents, "alpha-squad-coding-craft")
        self.skill(self.agents, "laya-model-advisor")
        with patch.dict("os.environ", {"CODEX_HOME": str(self.home)}):
            self.assertEqual(codex_home(), self.home)
        self.install()
        self.assertIn(str(alpha), (self.home / "AGENTS.md").read_text())

    def test_missing_or_duplicate_skill_blocks_write(self):
        with self.assertRaisesRegex(RuntimeError, "not installed"):
            self.install()
        self.prepare()
        self.skill(self.agents, "alpha-squad-coding-craft")
        with self.assertRaisesRegex(RuntimeError, "Ambiguous"):
            self.install()
        self.assertFalse((self.home / "AGENTS.md").exists())

    def test_symlink_or_override_blocks_write(self):
        self.prepare()
        real = Path(self.temp.name) / "original.md"
        real.write_text("unchanged")
        target = self.home / "AGENTS.md"
        target.symlink_to(real)
        with self.assertRaisesRegex(RuntimeError, "non-regular"):
            self.install()
        self.assertEqual(real.read_text(), "unchanged")
        target.unlink()
        (self.home / "AGENTS.override.md").write_text("override")
        with self.assertRaisesRegex(RuntimeError, "overrides"):
            self.install()

    def test_fenced_examples_crlf_and_duplicate_sections(self):
        rules = render_rules(Path("/a"), Path("/b"))
        original = f"```md\n{TITLE}\nexample\n```\n"
        self.assertTrue(replace_goal_section(original, rules).startswith(original))
        crlf = f"# Before\r\ntext\r\n{TITLE}\r\nold\r\n## After\r\ntail\r\n"
        updated = replace_goal_section(crlf, rules)
        self.assertNotIn("\n", updated.replace("\r\n", ""))
        with self.assertRaisesRegex(RuntimeError, "Multiple"):
            replace_goal_section(f"{TITLE}\na\n{TITLE}\nb", rules)

    def test_installer_only_writes_after_codex_install_with_opt_in(self):
        source = Path(__file__).resolve().parents[1]
        events = []
        with patch.object(installer, "ensure_platform"), \
                patch.object(installer, "detect_clients", return_value=[installer.Client("codex", "Codex", "codex")]), \
                patch.object(installer, "register_advisor_skill"), \
                patch.object(installer, "register_codex"), \
                patch.object(installer, "install_runtime", return_value=(Path("/bin/server"), Path("/models"))), \
                patch.object(installer, "register_alpha_squad_skill", side_effect=lambda dry_run: events.append(("alpha", dry_run))), \
                patch.object(installer, "install_goal_workflow", side_effect=lambda dry_run: events.append(("goal", dry_run))):
            installer.main(["--source-root", str(source), "--targets", "codex", "--goal-workflow", "yes"])
            self.assertEqual(events, [("goal", True), ("alpha", True), ("alpha", False), ("goal", False)])
            events.clear()
            installer.main(["--source-root", str(source), "--targets", "codex", "--goal-workflow", "no"])
            self.assertFalse(any(item[0] == "goal" for item in events))

    def test_failed_alpha_install_does_not_write_global_rules(self):
        source = Path(__file__).resolve().parents[1]
        with patch.object(installer, "ensure_platform"), \
                patch.object(installer, "detect_clients", return_value=[installer.Client("codex", "Codex", "codex")]), \
                patch.object(installer, "register_advisor_skill"), \
                patch.object(installer, "register_codex"), \
                patch.object(installer, "install_runtime", return_value=(Path("/bin/server"), Path("/models"))), \
                patch.object(installer, "register_alpha_squad_skill", side_effect=[True, RuntimeError("offline")]), \
                patch.object(installer, "install_goal_workflow") as goal:
            with self.assertRaisesRegex(RuntimeError, "offline"):
                installer.main(["--source-root", str(source), "--targets", "codex", "--goal-workflow", "yes"])
            goal.assert_called_once_with(dry_run=True)

    def test_goal_option_supports_claude_without_codex(self):
        with patch.object(installer, "ensure_platform"), \
                patch.object(installer, "detect_clients", return_value=[installer.Client("claude", "Claude", "claude")]), \
                patch.object(installer, "register_claude"), \
                patch.object(installer, "register_client_skills"), \
                patch.object(installer, "install_goal_workflow") as goal, \
                patch.object(installer, "install_runtime", return_value=(Path("/server"), Path("/model"))):
            installer.main(["--source-root", str(Path(__file__).resolve().parents[1]),
                            "--targets", "claude", "--goal-workflow", "yes"])
            self.assertEqual(goal.call_count, 2)
            goal.assert_called_with(dry_run=False, client="claude")


if __name__ == "__main__":
    unittest.main()
