from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch

from laya_tell_me.advisor import preferences
from laya_tell_me.routing import build_role_advice
from test_advisor import MODELS, result


PARENT = {"model": "strong-test", "reasoning_effort": "medium"}
EXECUTION = {"model": "fast-test", "reasoning_effort": "medium"}


class RoleRoutingTests(unittest.TestCase):
    def route(self, role="worker", raw=None, policy="auto", **overrides):
        settings = {
            "policy": policy, "needs_policy_selection": False, "ceiling": EXECUTION,
            "squad": {"enabled": True, "reviewer": EXECUTION},
        }
        settings.update(overrides)
        return build_role_advice(
            raw if raw is not None else result(), MODELS,
            PARENT["model"], PARENT["reasoning_effort"], role, settings,
            execution_choice=EXECUTION,
        )

    def test_execution_roles_stay_within_ceiling(self):
        for role in ("explorer", "worker", "tester", "researcher"):
            advice = self.route(role, result(risk="high"))
            self.assertEqual(advice["recommendation"], EXECUTION)
            self.assertEqual(advice["delegation"]["spawn_parameters"], {
                "agent_type": role, "model": "fast-test",
                "reasoning_effort": "medium", "fork_turns": "none",
            })
            self.assertFalse(advice["delegation"]["agent_spawned"])
            self.assertFalse(advice["delegation"]["execution_authorized"])
            self.assertFalse(advice["model_switched"])

    def test_reviewer_ordinary_vs_difficult(self):
        self.assertEqual(self.route("reviewer")["recommendation"], EXECUTION)
        for raw in (result(complexity="high"), result(risk="high"),
                    result(certainty="uncertain"), {}, result(confidence=0.1)):
            advice = self.route("reviewer", raw)
            self.assertEqual(advice["recommendation"], PARENT)
            self.assertTrue(advice["recommendation_accepted"])
        default = self.route("reviewer", squad={"enabled": True, "reviewer": None})
        self.assertEqual(default["recommendation"], PARENT)

    def test_confirmation_policy_still_applies(self):
        self.assertIsNone(self.route(policy="always")["delegation"]["spawn_parameters"])
        self.assertIsNone(self.route("reviewer", result(risk="high"), "conditional")
                          ["delegation"]["spawn_parameters"])
        self.assertIsNotNone(self.route("reviewer", policy="conditional")
                             ["delegation"]["spawn_parameters"])
        self.assertEqual(self.route(policy="conditional")["recommendation"], EXECUTION)

    def test_old_consent_and_invalid_ceiling_never_spawn(self):
        for overrides in ({"squad": None}, {"squad": {"enabled": False, "reviewer": None}},
                          {"ceiling": None}, {"ceiling": {"model": "missing", "reasoning_effort": "high"}}):
            advice = self.route(**overrides)
            self.assertTrue(advice["ask_user"])
            self.assertIsNone(advice["delegation"]["spawn_parameters"])

    def test_missing_current_pair_and_invalid_reviewer_never_spawn(self):
        advice = self.route("reviewer", squad={"enabled": True, "reviewer": {
            "model": "missing", "reasoning_effort": "ultra",
        }})
        self.assertTrue(advice["delegation"]["needs_verified_pair"])
        self.assertIsNone(advice["delegation"]["spawn_parameters"])
        advice = build_role_advice(result(), MODELS, "strong-test", None, "worker", {
            "policy": "auto", "needs_policy_selection": False, "ceiling": EXECUTION,
            "squad": {"enabled": True, "reviewer": None},
        })
        self.assertIsNone(advice["delegation"]["assignment"])

    def test_orchestrator_never_routed(self):
        with self.assertRaises(ValueError):
            self.route("orchestrator")

    def test_preferences_preserve_squad_and_old_policy(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "advisor.json"
            preferences("auto", path, ceiling=EXECUTION)
            self.assertFalse(preferences(path=path)["squad"]["enabled"])
            squad = {"enabled": True, "reviewer": PARENT}
            saved = preferences(path=path, squad=squad)
            self.assertEqual(saved["ceiling"], EXECUTION)
            self.assertEqual(saved["policy"], "auto")
            self.assertEqual(preferences("conditional", path)["squad"], squad)
            before = path.read_text()
            with self.assertRaises(ValueError):
                preferences(path=path, squad={"enabled": "yes", "reviewer": None})
            self.assertEqual(path.read_text(), before)

    def test_server_role_mode_and_validation(self):
        from laya_tell_me import server
        with TemporaryDirectory() as directory, patch.dict("os.environ", {
            "LAYA_ADVISOR_CONFIG": str(Path(directory) / "advisor.json")
        }), patch.object(server, "get_agent", return_value=Mock()) as get_agent:
            with self.assertRaises(ValueError):
                server.laya_tell_me("task", advisor={"models": MODELS, "role": "orchestrator"})
            get_agent.assert_not_called()
            with self.assertRaises(ValueError):
                server.laya_advisor_preferences(squad={"enabled": True, "reviewer": {
                    "model": "not-available", "reasoning_effort": "high",
                }}, models=MODELS)
            server.laya_advisor_preferences("auto", EXECUTION, MODELS,
                                            {"enabled": True, "reviewer": None})
            get_agent.return_value.predict.return_value = result()
            response = server.laya_tell_me("task", advisor={
                "models": MODELS, "role": "worker", "current_model": PARENT["model"],
                "current_reasoning_effort": PARENT["reasoning_effort"],
            })
            self.assertIsNotNone(response["advice"]["delegation"]["spawn_parameters"])


if __name__ == "__main__":
    unittest.main()
