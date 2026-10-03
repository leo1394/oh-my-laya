import importlib.util
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch


@unittest.skipUnless(importlib.util.find_spec("mcp"), "requires installed MCP dependency")
class ServerTests(unittest.TestCase):
    def setUp(self):
        from laya_tell_me import server
        self.server = server

    def test_original_tool_response_unchanged(self):
        raw = {"answers": {"test": {"choice": "low"}}}
        agent = Mock()
        agent.predict.return_value = raw
        questions = {"test": {"type": "choice", "instructions": "Classify", "criteria": ["low", "high"]}}
        with patch.object(self.server, "get_agent", return_value=agent):
            self.assertIs(self.server.laya_tell_me("task", questions), raw)
        agent.predict.assert_called_once_with("task", questions)

    def test_invalid_questions_rejected_before_loading_model(self):
        invalid = [None, {}, [], ["question"], {"test": "question"},
            {"test": {"type": "unknown", "instructions": "Classify"}},
            {"test": {"type": "choice", "criteria": ["low"]}},
            {"test": {"type": "choice", "instructions": "Classify", "options": ["low"]}},
            {"test": {"type": "choice", "instructions": "Classify", "criteria": []}},
            {"test": {"type": "choice", "instructions": "Classify", "criteria": ["low", "low"]}},
            {"test": {"type": "choice", "instructions": "Classify", "criteria": [1]}},
            {"test": {"type": "score", "instructions": "Score", "criteria": {"low": "Low"}}},
            {"test": {"type": "noul", "instructions": "Act?", "criteria": []}},
        ]
        for questions in invalid:
            with self.subTest(questions=questions), patch.object(self.server, "get_agent") as load:
                with self.assertRaises(ValueError):
                    self.server.run_prediction("task", questions)
                load.assert_not_called()

    def test_supported_question_shapes_pass_through(self):
        questions = {
            "choice": {"type": "choice", "instructions": "Classify", "criteria": {"low": "Small", "high": "Large"}},
            "score": {"type": "score", "instructions": "Score", "criteria": ["low", "high"]},
            "noul": {"type": "noul", "instructions": "Act?"},
        }
        agent = Mock()
        with patch.object(self.server, "get_agent", return_value=agent):
            self.server.run_prediction("task", questions)
        agent.predict.assert_called_once_with("task", questions)

    def test_advisor_uses_fixed_questions_and_keeps_catalog_out_of_inference(self):
        from laya_tell_me.advisor import ADVISOR_QUESTIONS
        agent = Mock()
        agent.predict.return_value = {"answers": {}}
        with TemporaryDirectory() as directory, patch.dict("os.environ", {
            "LAYA_ADVISOR_CONFIG": str(Path(directory) / "advisor.json")
        }), patch.object(self.server, "get_agent", return_value=agent):
            self.server.laya_advisor_preferences("conditional")
            response = self.server.laya_tell_me("task", advisor={"models": [
                {"id": "test-model", "reasoning_efforts": ["high"]}
            ], "current_model": "test-model"})
        agent.predict.assert_called_once_with("task", ADVISOR_QUESTIONS)
        self.assertTrue(response["advice"]["ask_user"])
        self.assertFalse(response["advice"]["model_switched"])
        self.assertIs(response["laya_result"], agent.predict.return_value)

    def test_invalid_advisor_rejected_before_loading_model(self):
        with patch.object(self.server, "get_agent") as get_agent:
            with self.assertRaises(ValueError):
                self.server.laya_tell_me("task", advisor={"models": [{}]})
            with self.assertRaises(ValueError):
                self.server.laya_tell_me("task", questions={}, advisor={})
            get_agent.assert_not_called()


    def test_ceiling_save_requires_supported_pair(self):
        with TemporaryDirectory() as directory, patch.dict("os.environ", {
            "LAYA_ADVISOR_CONFIG": str(Path(directory) / "advisor.json")
        }):
            ceiling = {"model": "test-model", "reasoning_effort": "high"}
            with self.assertRaises(ValueError):
                self.server.laya_advisor_preferences("auto", ceiling, [
                    {"id": "test-model", "reasoning_efforts": ["low"]}
                ])
            self.assertTrue(self.server.laya_advisor_preferences()["needs_policy_selection"])
            settings = self.server.laya_advisor_preferences("auto", ceiling, [
                {"id": "test-model", "reasoning_efforts": ["low", "high"]}
            ])
            self.assertEqual(settings["ceiling"], ceiling)


if __name__ == "__main__":
    unittest.main()
