from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from laya_tell_me.advisor import build_advice, preferences, validate_catalog
from laya_tell_me.installer import register_advisor_skill


def result(complexity="low", risk="low", certainty="clear", confidence=0.9):
    return {"answers": {
        key: {"choice": value, "confidence": confidence}
        for key, value in (("complexity", complexity), ("risk", risk), ("certainty", certainty))
    }}


MODELS = [
    {"id": "fast-test", "reasoning_efforts": ["low", "medium"], "tier": "fast"},
    {"id": "strong-test", "reasoning_efforts": ["medium", "high"], "tier": "strong"},
]


class AdvisorTests(unittest.TestCase):
    def advise(self, raw=None, policy="always", models=None, configured=True):
        return build_advice(
            raw if raw is not None else result(), MODELS if models is None else models,
            "fast-test", {"policy": policy, "needs_policy_selection": not configured,
                          "ceiling": {"model": "fast-test", "reasoning_effort": "medium"}},
        )

    def test_policy_matrix(self):
        for policy in ("always", "conditional", "auto"):
            for raw, exceptional in (
                (result(), False), (result(complexity="medium"), False),
                (result(complexity="high"), True), (result(risk="high"), True),
                (result(certainty="uncertain"), True), (result(confidence=0.59), True),
            ):
                with self.subTest(policy=policy, raw=raw):
                    advice = self.advise(raw, policy)
                    ask = policy == "always" or (policy == "conditional" and exceptional)
                    self.assertEqual(advice["ask_user"], ask)
                    self.assertEqual(advice["recommendation_accepted"], not ask)
                    self.assertFalse(advice["model_switched"])

    def test_unconfigured_auto_still_requires_selection(self):
        self.assertTrue(self.advise(policy="auto", configured=False)["ask_user"])

    def test_invalid_answers_are_uncertain(self):
        for raw in ({}, {"answers": None}, {"answers": {"risk": []}},
                    result(risk="bogus"), result(confidence=float("nan")),
                    result(confidence=2), result(confidence=True)):
            self.assertTrue(self.advise(raw, "conditional")["ask_user"])

    def test_conservative_threshold_boundary(self):
        self.assertFalse(self.advise(result(confidence=0.6), "conditional")["ask_user"])

    def test_high_risk_uses_strong_tier_and_supported_effort(self):
        self.assertEqual(self.advise(result(risk="high"))["recommendation"],
                         {"model": "strong-test", "reasoning_effort": "high"})

    def test_no_mapping_keeps_current_model(self):
        models = [{"id": "fast-test", "reasoning_efforts": ["minimal", "max"]}]
        advice = self.advise(result(risk="high"), models=models)
        self.assertEqual(advice["recommendation"],
                         {"model": "fast-test", "reasoning_effort": "max"})

    def test_unknown_current_and_no_mapping_does_not_pick_arbitrary_model(self):
        advice = self.advise(policy="auto", models=[
            {"id": "other", "reasoning_efforts": ["medium"]}
        ])
        self.assertIsNone(advice["recommendation"])
        self.assertFalse(advice["recommendation_accepted"])

    def test_missing_catalog_does_not_claim_acceptance(self):
        advice = self.advise(policy="auto", models=[])
        self.assertIsNone(advice["recommendation"])
        self.assertFalse(advice["recommendation_accepted"])
        self.assertEqual(advice["status"], "awaiting_user")
        self.assertTrue(advice["needs_policy_selection"])

    def test_invalid_catalog(self):
        for models in (None, [{}], [MODELS[0], MODELS[0]],
                       [{"id": "test", "reasoning_efforts": []}],
                       [{"id": "test", "reasoning_efforts": ["high"], "tier": "bogus"}]):
            with self.assertRaises(ValueError):
                validate_catalog(models)

    def test_preferences_read_save_and_corruption(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "advisor.json"
            self.assertTrue(preferences(path=path)["needs_policy_selection"])
            self.assertFalse(path.exists())
            for policy in ("always", "conditional", "auto"):
                ceiling = {"model": "test", "reasoning_effort": "high"} if policy == "auto" else None
                self.assertEqual(preferences(policy, path, ceiling=ceiling)["policy"], policy)
                self.assertFalse(preferences(path=path)["needs_policy_selection"])
            with self.assertRaises(ValueError):
                preferences("bad", path)
            self.assertEqual(preferences(path=path)["policy"], "auto")
            path.write_text("invalid")
            self.assertTrue(preferences(path=path)["needs_policy_selection"])

    def test_skill_install_dry_run_update_and_protect_local_edits(self):
        source = Path(__file__).resolve().parents[1]
        with TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "laya-model-advisor" / "SKILL.md"
            register_advisor_skill(source, True, root)
            self.assertFalse(target.exists())
            register_advisor_skill(source, False, root)
            register_advisor_skill(source, False, root)
            target.write_text("user edit")
            with self.assertRaisesRegex(RuntimeError, "local changes"):
                register_advisor_skill(source, False, root)
            self.assertEqual(target.read_text(), "user edit")

    def test_unmanaged_skill_is_not_overwritten(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "laya-model-advisor").mkdir()
            with self.assertRaisesRegex(RuntimeError, "Unmanaged"):
                register_advisor_skill(Path(__file__).resolve().parents[1], True, root)


class CeilingTests(unittest.TestCase):
    def test_auto_pins_model_and_caps_effort(self):
        settings = {"policy": "auto", "needs_policy_selection": False,
                    "ceiling": {"model": "fast-test", "reasoning_effort": "medium"}}
        advice = build_advice(result(risk="high"), MODELS, "strong-test", settings)
        self.assertEqual(advice["recommendation"],
                         {"model": "fast-test", "reasoning_effort": "medium"})
        self.assertTrue(advice["recommendation_accepted"])
        self.assertFalse(advice["model_switched"])
        low = build_advice(result(), MODELS, "strong-test", settings)
        self.assertEqual(low["recommendation"]["reasoning_effort"], "low")

    def test_missing_or_unavailable_ceiling_requires_setup(self):
        for ceiling in (None, {}, {"model": "missing", "reasoning_effort": "high"},
                        {"model": "fast-test", "reasoning_effort": "ultra"}):
            advice = build_advice(result(), MODELS, "fast-test", {
                "policy": "auto", "needs_policy_selection": False, "ceiling": ceiling,
            })
            self.assertTrue(advice["ask_user"])
            self.assertTrue(advice["needs_policy_selection"])
            self.assertFalse(advice["recommendation_accepted"])
            self.assertIsNone(advice["recommendation"])

    def test_migration_and_atomic_ceiling(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "advisor.json"
            path.write_text('{"policy": "auto"}')
            self.assertTrue(preferences(path=path)["needs_policy_selection"])
            ceiling = {"model": "test", "reasoning_effort": "high"}
            settings = preferences("auto", path, ceiling=ceiling)
            self.assertFalse(settings["needs_policy_selection"])
            self.assertEqual(preferences(path=path)["ceiling"], ceiling)
            before = path.read_text()
            with self.assertRaises(ValueError):
                preferences("auto", path, ceiling={"model": "test"})
            self.assertEqual(path.read_text(), before)
            self.assertIsNone(preferences("always", path)["ceiling"])


if __name__ == "__main__":
    unittest.main()
