from copy import deepcopy
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch

from laya_tell_me.orchestration import build_plan, validate_context


def context(**changes):
    return {"schema_version": 1, "enabled": True, "run_id": "run-1", "stage_id": "stage-1",
            "snapshot_revision": "rev-1", "independent_work": False, "dependencies_known": True,
            "required_roles": [], "constraint_refs": ["user-requirements"], **changes}


def advice(complexity="low", risk="low", uncertain=False):
    return {"uncertain": uncertain, "assessment": {
        "complexity": {"choice": complexity}, "risk": {"choice": risk},
        "certainty": {"choice": "uncertain" if uncertain else "clear"},
    }}


class OrchestrationTests(unittest.TestCase):
    def test_simple_task_is_direct_without_execution_authority(self):
        plan = build_plan(advice(), context(), {"task": "README typo"})
        self.assertEqual(plan["mode"], "direct")
        self.assertEqual(plan["required_roles"], [])
        self.assertFalse(plan["execution_authorized"])
        self.assertFalse(plan["parent_model_switched"])
        self.assertIsNone(plan["decision_id"])
        self.assertEqual(plan["enforcement"], "advisory")

    def test_only_needed_roles_and_high_risk_review(self):
        plan = build_plan(advice("medium"), context(independent_work=True), "task")
        self.assertEqual(plan["required_roles"], ["worker"])
        self.assertEqual(plan["mode"], "delegate")
        plan = build_plan(advice(risk="high"), context(required_roles=["tester"]), "task")
        self.assertEqual(plan["required_roles"], ["tester", "reviewer"])
        self.assertEqual(plan["mode"], "delegate")
        plan = build_plan(advice(), context(required_roles=["reviewer"]), "task")
        self.assertEqual(plan["mode"], "delegate")

    def test_uncertainty_preserves_obligations_without_spawning(self):
        for input_advice in (advice(uncertain=True), {}, advice(complexity="invalid")):
            self.assertEqual(build_plan(input_advice, context(), "task")["mode"], "needs_context")
        plan = build_plan(advice(risk="high", uncertain=True), context(), "task")
        self.assertEqual(plan["mode"], "needs_context")
        self.assertEqual(plan["required_roles"], ["reviewer"])
        self.assertEqual(build_plan(advice(), context(dependencies_known=False), "task")["mode"], "needs_context")

    def test_fingerprint_is_canonical_and_context_is_not_mutated(self):
        value = context()
        original = deepcopy(value)
        first = build_plan(advice(), value, {"a": 1, "b": 2})
        second = build_plan(advice(), value, {"b": 2, "a": 1})
        self.assertEqual(first["input_fingerprint"], second["input_fingerprint"])
        self.assertEqual(value, original)
        self.assertNotEqual(first["input_fingerprint"], build_plan(advice(), value, "changed")["input_fingerprint"])

    def test_strict_context_validation(self):
        for changes in ({"schema_version": True}, {"enabled": 1}, {"run_id": " "},
                        {"required_roles": ["orchestrator"]}, {"required_roles": ["worker", "worker"]},
                        {"constraint_refs": [1]}, {"unexpected": True}, {"independent_work": "false"}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                validate_context(context(**changes))
        self.assertIsNone(build_plan(advice(), context(enabled=False), "task"))

    def test_prediction_is_single_call_and_disabled_response_unchanged(self):
        from laya_tell_me import server
        from laya_tell_me.advisor import ADVISOR_QUESTIONS
        raw = {"answers": {key: {"choice": "clear" if key == "certainty" else "low", "confidence": 0.9}
                           for key in ADVISOR_QUESTIONS}}
        model = Mock()
        model.predict.return_value = raw
        catalog = {"models": [{"id": "test", "reasoning_efforts": ["medium"]}], "current_model": "test"}
        with TemporaryDirectory() as temporary, patch.dict("os.environ", {
            "LAYA_ADVISOR_CONFIG": str(Path(temporary) / "advisor.json")
        }), patch.object(server, "get_agent", return_value=model):
            result = server.run_prediction("README typo", advisor=catalog, orchestration=context())
            model.predict.assert_called_once_with("README typo", ADVISOR_QUESTIONS)
            self.assertEqual(result["orchestration_plan"]["mode"], "direct")
            legacy = server.run_prediction("README typo", advisor=catalog)
            disabled = server.run_prediction("README typo", advisor=catalog, orchestration=context(enabled=False))
            self.assertEqual(legacy, disabled)
            self.assertNotIn("orchestration_plan", legacy)

    def test_invalid_context_rejected_before_model_load(self):
        from laya_tell_me import server
        with patch.object(server, "get_agent") as load:
            with self.assertRaises(ValueError):
                server.run_prediction("task", advisor={}, orchestration=context(enabled=1))
            with self.assertRaises(ValueError):
                server.run_prediction("task", orchestration=context())
            load.assert_not_called()

    def test_direct_gate_suppresses_role_spawn_parameters(self):
        from laya_tell_me import server
        role_advice = {**advice(), "delegation": {"spawn_parameters": {"agent_type": "worker"}}}
        with patch.object(server, "get_agent", return_value=Mock()), \
                patch.object(server, "preferences", return_value={}), \
                patch.object(server, "build_role_advice", return_value=role_advice):
            result = server.run_prediction("typo", advisor={"models": [], "role": "worker"}, orchestration=context())
        self.assertIsNone(result["advice"]["delegation"]["spawn_parameters"])
        self.assertEqual(result["advice"]["delegation"]["orchestration_gate"], "direct")


if __name__ == "__main__":
    unittest.main()
