from contextlib import ExitStack
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch, call

from laya_tell_me import installer
from laya_tell_me.goal_workflow import CLIENTS, client_home, install_goal_workflow


class ClientGoalTests(unittest.TestCase):
    def test_full_install_all_clients_in_isolated_home(self):
        with TemporaryDirectory() as temp, ExitStack() as stack:
            root = Path(temp).resolve()
            source = Path(__file__).resolve().parents[1]
            alpha = root / "upstream" / "alpha-squad-coding-craft"
            alpha.mkdir(parents=True)
            (alpha / "SKILL.md").write_text("upstream skill")
            homes = {key: root / key for key in CLIENTS}
            stack.enter_context(patch.dict("os.environ", {
                CLIENTS[key][0]: str(home) for key, home in homes.items()
            }))
            stack.enter_context(patch("pathlib.Path.home", return_value=root))
            stack.enter_context(patch.object(installer, "ensure_platform"))
            stack.enter_context(patch.object(installer, "detect_clients", return_value=[
                installer.Client(key, key, key) for key in CLIENTS
            ]))
            stack.enter_context(patch.object(installer, "install_runtime", return_value=(root / "server", root / "model")))
            stack.enter_context(patch.object(installer, "_download_alpha_squad_skill", return_value=alpha))
            commands = stack.enter_context(patch.object(installer, "run"))
            capability_probe = stack.enter_context(patch("laya_tell_me.codex_plugin.subprocess.run"))
            installer.main(["--source-root", str(source), "--targets", "all", "--goal-workflow", "yes",
                            "--install-dir", str(root / "runtime")])
            for key, home in homes.items():
                self.assertTrue((home / CLIENTS[key][2]).is_file())
                self.assertTrue((home / "skills" / "alpha-squad-coding-craft" / "SKILL.md").is_file())
                advisor_root = root / ".agents" if key == "codex" else home
                self.assertTrue((advisor_root / "skills" / "laya-model-advisor" / "SKILL.md").is_file())
            self.assertTrue((homes["dsh"] / "cordis.patch.yml").is_file())
            self.assertTrue((homes["pi"] / "prompts" / "goal.md").is_file())
            self.assertTrue((root / "runtime" / "pi-package" / "laya.config.json").is_file())
            self.assertTrue(commands.called)
            capability_probe.assert_called_once_with(
                ["codex", "plugin", "add", "--help"], check=True, capture_output=True
            )

    def test_each_client_writes_only_its_file_with_native_paths(self):
        with TemporaryDirectory() as temp:
            for client in CLIENTS:
                with self.subTest(client=client):
                    home = Path(temp).resolve() / client
                    for name in ("alpha-squad-coding-craft", "laya-model-advisor"):
                        directory = home / "skills" / name
                        directory.mkdir(parents=True)
                        (directory / "SKILL.md").write_text("skill")
                    target = home / CLIENTS[client][2]
                    target.write_text("## Existing\nkeep me\n")
                    install_goal_workflow(client=client, home=home, agents_skills=home / "skills")
                    text = target.read_text()
                    self.assertTrue(text.startswith("## Existing\nkeep me\n"))
                    self.assertIn(str(home / "skills" / "alpha-squad-coding-craft" / "SKILL.md"), text)
                    if client != "codex":
                        self.assertNotIn("codex_internal_context", text)
                    if client == "claude":
                        self.assertFalse((home / "AGENTS.md").exists())
                    if client == "pi":
                        prompt = home / "prompts" / "goal.md"
                        self.assertIn("$ARGUMENTS", prompt.read_text())
                        self.assertIn("not a native background Goal loop", prompt.read_text())
                    install_goal_workflow(client=client, home=home, agents_skills=home / "skills")
                    self.assertEqual(target.read_text(), text)

    def test_custom_home_environment_and_dry_run(self):
        with TemporaryDirectory() as temp:
            for client, (variable, _, filename) in CLIENTS.items():
                home = Path(temp).resolve() / client
                with patch.dict("os.environ", {variable: str(home)}):
                    self.assertEqual(client_home(client), home)
                    install_goal_workflow(True, client=client, agents_skills=home / "skills")
                    self.assertFalse((home / filename).exists())
                    self.assertFalse(home.exists())

    def test_pi_unmanaged_prompt_blocks_before_global_write(self):
        with TemporaryDirectory() as temp:
            home = Path(temp)
            (home / "prompts").mkdir()
            (home / "prompts" / "goal.md").write_text("user prompt")
            with self.assertRaisesRegex(RuntimeError, "unmanaged"):
                install_goal_workflow(True, client="pi", home=home, agents_skills=home / "skills")
            self.assertFalse((home / "AGENTS.md").exists())

    def test_installer_all_prompts_independently_and_writes_only_opted_in(self):
        with ExitStack() as stack:
            stack.enter_context(patch.object(installer, "ensure_platform"))
            stack.enter_context(patch.object(installer, "detect_clients", return_value=[
                installer.Client(key, key, key) for key in CLIENTS
            ]))
            for name in ("register_codex", "register_claude", "register_dsh", "register_pi"):
                stack.enter_context(patch.object(installer, name))
            skills = stack.enter_context(patch.object(installer, "register_client_skills"))
            goal = stack.enter_context(patch.object(installer, "configure_goal"))
            prompt = stack.enter_context(patch.object(installer, "select_goal_workflow", side_effect=[True, False, True, False]))
            stack.enter_context(patch.object(installer, "install_runtime", return_value=(Path("/server"), Path("/models"))))
            installer.main(["--source-root", str(Path(__file__).resolve().parents[1]), "--targets", "all"])
            ordered = sorted(CLIENTS)
            self.assertEqual(prompt.call_args_list, [call(None, False, client=key) for key in ordered])
            self.assertEqual(goal.call_args_list, [call(ordered[0], True), call(ordered[2], True),
                                                    call(ordered[0], False), call(ordered[2], False)])
            self.assertEqual(skills.call_count, 8)

    def test_other_clients_install_skills_into_own_home(self):
        with TemporaryDirectory() as temp:
            for client in ("claude", "dsh", "pi"):
                root = Path(temp) / client
                with patch.object(installer, "client_home", return_value=root), \
                        patch.object(installer, "register_advisor_skill") as advisor, \
                        patch.object(installer, "register_alpha_squad_skill") as alpha:
                    installer.register_client_skills(Path("/source"), True, client)
                    advisor.assert_called_once_with(Path("/source"), True, root / "skills")
                    alpha.assert_called_once_with(True, root / "skills", root / "skills")


if __name__ == "__main__":
    unittest.main()
