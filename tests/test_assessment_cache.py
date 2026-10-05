from copy import deepcopy
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch

from laya_tell_me.assessment_cache import AssessmentCache, assessment_key


def orchestration(**changes):
    value = {
        "schema_version": 1, "enabled": True, "run_id": "run-1",
        "stage_id": "stage-1", "snapshot_revision": "revision-1",
        "independent_work": False, "dependencies_known": True,
        "required_roles": ["worker", "reviewer"], "constraint_refs": ["scope"],
    }
    value.update(changes)
    return value


def settings(**changes):
    value = {
        "policy": "auto", "needs_policy_selection": False,
        "ceiling": {"model": "fixture-model", "reasoning_effort": "medium"},
        "squad": {"enabled": True, "reviewer": {
            "model": "fixture-model", "reasoning_effort": "low",
        }},
        "policies": ["always", "conditional", "auto"],
        "scope": "model recommendations only; not execution permissions",
    }
    value.update(changes)
    return value


def cache_params(role="worker", **changes):
    value = {
        "state": {"task": "bounded change"},
        "questions": {"risk": {"type": "choice", "instructions": "Risk?",
                                 "criteria": ["low", "high"]}},
        "advisor": {
            "models": [{"id": "fixture-model", "reasoning_efforts": ["low", "medium"]}],
            "current_model": "fixture-model", "current_reasoning_effort": "medium",
            "execution_choice": {"model": "fixture-model", "reasoning_effort": "medium"},
            "role": role,
        },
        "memory_cases": [{"id": "case-1", "summary": "prior", "labels": {"risk": "low"}}],
        "model_tiers": {"low": {"model": "fixture-model", "reasoning_effort": "low"}},
        "orchestration": orchestration(),
        "_cache_context": {
            "decision_id": "decision-1", "service_instance": "service-1", "epoch": 0,
            "memory_version": "memory-1", "settings": {"recording_enabled": False},
        },
    }
    value.update(changes)
    return value


def worker_info(**changes):
    value = {
        "loaded": True,
        "model": {
            "path": "test-only-fixture", "model_revision": "checkpoint-1",
            "checkpoint_digest": "checkpoint-1",
            "identity_scope": "full_checkpoint_content", "dtype": "fixture",
            "device": "fixture", "batch_size": 1,
        },
        "advisor_questions": {"risk": {"criteria": ["low", "high"]}},
        "rules_version": "rules-1",
        "supported_contracts": ["orchestration_plan_v1", "assessment_reuse_v1"],
    }
    value.update(changes)
    return value


class AssessmentCacheTests(unittest.TestCase):
    def test_fixed_ttl_lru_capacity_and_deep_copy(self):
        now = [0]
        cache = AssessmentCache(capacity=128, ttl=10, clock=lambda: now[0])
        original = {"answers": {"risk": {"choice": "low"}}}
        cache.put("fixed", original)
        original["answers"]["risk"]["choice"] = "high"
        now[0] = 5
        first = cache.get("fixed")
        self.assertEqual(first["answers"]["risk"]["choice"], "low")
        first["answers"]["risk"]["choice"] = "high"
        self.assertEqual(cache.get("fixed")["answers"]["risk"]["choice"], "low")
        now[0] = 10
        self.assertIsNone(cache.get("fixed"), "reads must not refresh the creation TTL")

        now[0] = 20
        for index in range(128):
            cache.put(str(index), {"index": index})
        self.assertEqual(len(cache.entries), 128)
        self.assertEqual(cache.get("0"), {"index": 0})
        cache.put("128", {"index": 128})
        self.assertIsNone(cache.get("1"))
        self.assertEqual(cache.get("0"), {"index": 0})
        self.assertEqual(len(cache.entries), 128)

    def test_key_covers_every_trust_and_input_dimension_except_role(self):
        params = cache_params()
        info = worker_info()
        preferences = settings()
        base = assessment_key(params, info, preferences)
        self.assertIsNotNone(base)

        mutations = {
            "run": ("params", ("orchestration", "run_id"), "run-2"),
            "stage": ("params", ("orchestration", "stage_id"), "stage-2"),
            "state": ("params", ("state", "task"), "different"),
            "snapshot": ("params", ("orchestration", "snapshot_revision"), "revision-2"),
            "questions": ("params", ("questions", "risk", "instructions"), "Changed risk?"),
            "checkpoint": ("info", ("model", "checkpoint_digest"), "checkpoint-2"),
            "rules": ("info", ("rules_version",), "rules-2"),
            "catalog": ("params", ("advisor", "models", 0, "id"), "other-model"),
            "preferences": ("settings", ("policy",), "conditional"),
            "tiers": ("params", ("model_tiers", "low", "reasoning_effort"), "medium"),
            "cases": ("params", ("memory_cases", 0, "id"), "case-2"),
            "memory_version": ("params", ("_cache_context", "memory_version"), "memory-2"),
            "epoch": ("params", ("_cache_context", "epoch"), 1),
            "service_instance": ("params", ("_cache_context", "service_instance"), "service-2"),
        }
        for label, (target, path, replacement) in mutations.items():
            values = {"params": deepcopy(params), "info": deepcopy(info),
                      "settings": deepcopy(preferences)}
            cursor = values[target]
            for part in path[:-1]:
                cursor = cursor[part]
            cursor[path[-1]] = replacement
            with self.subTest(label=label):
                self.assertNotEqual(base, assessment_key(
                    values["params"], values["info"], values["settings"]
                ))

        role = deepcopy(params)
        role["advisor"]["role"] = "reviewer"
        role["_cache_context"]["decision_id"] = "decision-2"
        self.assertEqual(base, assessment_key(role, info, preferences))

    def test_key_requires_enabled_orchestration_and_verified_loaded_identity(self):
        params = cache_params()
        info = worker_info()
        self.assertIsNone(assessment_key({**params, "orchestration": None}, info, settings()))
        self.assertIsNone(assessment_key(
            {**params, "orchestration": orchestration(enabled=False)}, info, settings()
        ))
        unknown = deepcopy(info)
        unknown["model"]["identity_scope"] = "unknown_loaded_checkpoint"
        unknown["model"]["checkpoint_digest"] = None
        self.assertIsNone(assessment_key(params, unknown, settings()))
        unloaded = deepcopy(info)
        unloaded["loaded"] = False
        self.assertIsNone(assessment_key(params, unloaded, settings()))


class WorkerAssessmentCacheTests(unittest.TestCase):
    def setUp(self):
        from laya_tell_me import worker
        self.worker = worker
        self.original_agent = worker.server._agent
        self.original_loaded_agent_id = worker._loaded_agent_id
        self.original_loaded_model = worker._loaded_model
        worker._assessment_cache.clear()

    def tearDown(self):
        self.worker._assessment_cache.clear()
        self.worker.server._agent = self.original_agent
        self.worker._loaded_agent_id = self.original_loaded_agent_id
        self.worker._loaded_model = self.original_loaded_model

    def bind_verified_agent(self):
        agent = Mock()
        agent.predict.return_value = {"answers": {
            "complexity": {"choice": "low", "confidence": 0.99},
            "risk": {"choice": "low", "confidence": 0.99},
            "certainty": {"choice": "clear", "confidence": 0.99},
        }}
        self.worker.server._agent = agent
        self.worker._loaded_agent_id = id(agent)
        self.worker._loaded_model = deepcopy(worker_info()["model"])
        return agent

    def worker_params(self, role, decision_id):
        value = cache_params(role)
        value.pop("questions")
        value["memory_cases"] = []
        value["_cache_context"]["decision_id"] = decision_id
        return value

    def test_hit_reuses_raw_assessment_but_reroutes_role_without_model_load(self):
        agent = self.bind_verified_agent()
        current_settings = settings()
        with TemporaryDirectory() as directory, patch.dict("os.environ", {
            "LAYA_ADVISOR_CONFIG": str(Path(directory) / "advisor.json")
        }), patch.object(self.worker.server, "preferences",
                        side_effect=lambda: deepcopy(current_settings)), patch.object(
            self.worker.server, "get_agent", return_value=agent
        ) as load:
            first = self.worker._predict(self.worker_params("worker", "decision-1"))
            second = self.worker._predict(self.worker_params("reviewer", "decision-2"))

        agent.predict.assert_called_once()
        load.assert_called_once()
        self.assertEqual(first["meta"]["assessment_cache"]["status"], "miss")
        self.assertEqual(second["meta"]["assessment_cache"], {
            "status": "hit", "inference_input_tokens": 0,
            "inference_output_tokens": 0, "raw_usage_scope": "source_evaluation",
        })
        self.assertEqual(second["meta"]["reused_from_decision_id"], "decision-1")
        self.assertEqual(second["orchestration_plan"]["reused_from_decision_id"], "decision-1")
        self.assertEqual(first["advice"]["delegation"]["role"], "worker")
        self.assertEqual(second["advice"]["delegation"]["role"], "reviewer")

        self.worker._dispatch("clear_assessment_cache", {})
        with patch.object(self.worker.server, "preferences",
                          side_effect=lambda: deepcopy(current_settings)):
            third = self.worker._predict(self.worker_params("reviewer", "decision-3"))
        self.assertEqual(agent.predict.call_count, 2)
        self.assertEqual(third["meta"]["assessment_cache"]["status"], "miss")

    def test_unknown_identity_disabled_and_unset_context_never_cache(self):
        agent = self.bind_verified_agent()
        current_settings = settings()
        with patch.object(self.worker.server, "preferences",
                          side_effect=lambda: deepcopy(current_settings)):
            self.worker._loaded_model["checkpoint_digest"] = None
            self.worker._loaded_model["identity_scope"] = "unknown_loaded_checkpoint"
            unknown_first = self.worker._predict(self.worker_params("worker", "unknown-1"))
            unknown_second = self.worker._predict(self.worker_params("reviewer", "unknown-2"))
            self.assertEqual(unknown_first["meta"]["assessment_cache"]["status"], "unavailable")
            self.assertEqual(unknown_second["meta"]["assessment_cache"]["status"], "unavailable")

            self.worker._loaded_model = deepcopy(worker_info()["model"])
            disabled = self.worker_params("worker", "disabled")
            disabled["orchestration"] = orchestration(enabled=False)
            self.assertEqual(self.worker._predict(disabled)["meta"]["assessment_cache"]["status"],
                             "unavailable")
            unset = self.worker_params("worker", "unset")
            unset.pop("_cache_context")
            result = self.worker._predict(unset)
            self.assertNotIn("assessment_cache", result.get("meta", {}))

        self.assertEqual(agent.predict.call_count, 4)


if __name__ == "__main__":
    unittest.main()
