from copy import deepcopy
import json
from pathlib import Path
import re
import unittest

import jsonschema


ROOT = Path(__file__).resolve().parents[1]
ROOT_SCHEMA = ROOT / "contracts/feedback.schema.json"
PORTABLE_REFERENCE = (
    ROOT / "skills/alpha-squad-coding-craft/skills/alpha-squad-coding-craft"
    / "references/laya-feedback.md"
)


def portable_schema():
    text = PORTABLE_REFERENCE.read_text()
    match = re.search(
        r"<!-- laya-feedback-schema:start -->\s*```json\s*(.*?)\s*```\s*"
        r"<!-- laya-feedback-schema:end -->",
        text, re.DOTALL,
    )
    if match is None:
        raise AssertionError("portable feedback schema is missing")
    return json.loads(match.group(1))


def dispatch_receipt():
    return {
        "contract": "dispatch_receipt_v1", "run_id": "run-1", "stage_id": "stage-1",
        "ordinal": 1, "policy_version": "bounded-attempts-v1", "enforcement": "advisory",
        "role": "worker", "attempt_kind": "initial", "status": "started",
        "native_execution_ref": "native:1",
        "context_isolation": "isolated", "context_evidence_ref": "context:1",
        "input_size": {"value": 100, "unit": "native_tokens", "source": "host-meter"},
    }


def assignment_event():
    return {
        "protocol_version": 1, "event_id": "dispatch", "decision_id": "decision",
        "attempt_ref": "attempt", "kind": "assignment",
        "source": {"host": "codex", "role": "worker", "actor_type": "agent"},
        "payload": {
            "recommended": None, "selected": None, "requested": None, "effective": None,
            "reason": "bounded dispatch", "evidence_refs": [], "execution": dispatch_receipt(),
        },
    }


def outcome_receipt():
    return {
        "contract": "attempt_outcome_v1", "run_id": "run-1", "stage_id": "stage-1",
        "ordinal": 1, "policy_version": "bounded-attempts-v1", "enforcement": "advisory",
        "dispatch_event_id": "dispatch", "result": "success", "failure_class": None,
        "first_feedback_event_id": None, "test_event_ids": ["test-1"],
        "review_event_ids": ["review-1"], "usage_event_ids": ["usage-1"], "duration_ms": 10,
    }


def outcome_event():
    return {
        "protocol_version": 1, "event_id": "outcome", "decision_id": "decision",
        "attempt_ref": "attempt", "kind": "outcome",
        "source": {"host": "codex", "role": "worker", "actor_type": "agent"},
        "payload": {"outcome": "success", "status": "success", "summary": "done",
                    "execution": outcome_receipt()},
    }


class ExecutionSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        schemas = [json.loads(ROOT_SCHEMA.read_text()), portable_schema()]
        for schema in schemas:
            jsonschema.Draft202012Validator.check_schema(schema)
        cls.live = jsonschema.Draft202012Validator(schemas[0])
        cls.portable = jsonschema.Draft202012Validator(schemas[1])
        cls.validators = [cls.live, cls.portable]

    def assert_valid_in_both(self, event):
        for validator in self.validators:
            with self.subTest(schema=validator.schema.get("$id")):
                validator.validate(event)

    def assert_invalid_in_both(self, event):
        for validator in self.validators:
            with self.subTest(schema=validator.schema.get("$id")):
                with self.assertRaises(jsonschema.ValidationError):
                    validator.validate(event)

    def assert_invalid_live(self, event):
        with self.assertRaises(jsonschema.ValidationError):
            self.live.validate(event)

    def assert_invalid_portable(self, event):
        with self.assertRaises(jsonschema.ValidationError):
            self.portable.validate(event)

    def test_dispatch_and_outcome_receipts_have_validation_parity(self):
        self.assert_valid_in_both(assignment_event())
        self.assert_valid_in_both(outcome_event())

        unknown = assignment_event()
        unknown["payload"]["execution"].update({
            "attempt_kind": "manual", "status": "unknown", "native_execution_ref": None,
            "context_isolation": "unsupported", "context_evidence_ref": None,
            "input_size": {"value": None, "unit": "unknown", "source": None},
        })
        self.assert_valid_in_both(unknown)

        failed = outcome_event()
        failed["payload"]["outcome"] = "failure"
        failed["payload"]["status"] = "failure"
        failed["payload"]["execution"]["result"] = "failure"
        failed["payload"]["execution"]["failure_class"] = "acceptance"
        self.assert_valid_in_both(failed)

    def test_malformed_common_and_dispatch_fields_fail_in_live_schema(self):
        cases = []
        for field, value in [
            ("contract", "wrong"), ("run_id", " "), ("stage_id", "x" * 129),
            ("ordinal", 0), ("ordinal", 1_000_001), ("ordinal", True),
            ("policy_version", "other"), ("enforcement", "automatic"),
            ("role", "parent"), ("attempt_kind", "retry"), ("status", "completed"),
        ]:
            event = assignment_event()
            event["payload"]["execution"][field] = value
            cases.append(event)
        missing = assignment_event()
        del missing["payload"]["execution"]["stage_id"]
        cases.append(missing)
        missing_role = assignment_event()
        del missing_role["payload"]["execution"]["role"]
        cases.append(missing_role)
        extra = assignment_event()
        extra["payload"]["execution"]["authorize"] = True
        cases.append(extra)
        native = assignment_event()
        native["payload"]["execution"]["native_execution_ref"] = None
        cases.append(native)
        isolation = assignment_event()
        isolation["payload"]["execution"]["context_evidence_ref"] = None
        cases.append(isolation)
        measured = assignment_event()
        measured["payload"]["execution"]["input_size"] = {
            "value": 10, "unit": "unknown", "source": None,
        }
        cases.append(measured)
        for index, event in enumerate(cases):
            with self.subTest(case=index):
                self.assert_invalid_live(event)

    def test_malformed_outcome_fields_fail_in_live_schema(self):
        cases = []
        success_failure = outcome_event()
        success_failure["payload"]["execution"]["failure_class"] = "risk"
        cases.append(success_failure)
        duplicate = outcome_event()
        duplicate["payload"]["execution"]["test_event_ids"] = ["test-1", "test-1"]
        cases.append(duplicate)
        too_many = outcome_event()
        too_many["payload"]["execution"]["usage_event_ids"] = [f"usage-{index}" for index in range(33)]
        cases.append(too_many)
        duration = outcome_event()
        duration["payload"]["execution"]["duration_ms"] = -1
        cases.append(duration)
        blank_ref = outcome_event()
        blank_ref["payload"]["execution"]["dispatch_event_id"] = ""
        cases.append(blank_ref)
        no_alias = outcome_event()
        del no_alias["payload"]["outcome"]
        del no_alias["payload"]["status"]
        cases.append(no_alias)
        alias = outcome_event()
        alias["payload"]["status"] = "failure"
        cases.append(alias)
        for index, event in enumerate(cases):
            with self.subTest(case=index):
                self.assert_invalid_live(event)

    def test_portable_schema_checks_execution_discriminator_and_existing_invariants(self):
        wrong_contract = assignment_event()
        wrong_contract["payload"]["execution"]["contract"] = "other"
        wrong_contract["payload"]["scores"] = []
        self.assert_invalid_portable(wrong_contract)

        wrong_type = assignment_event()
        wrong_type["payload"]["execution"] = "dispatch_receipt_v1"
        self.assert_invalid_portable(wrong_type)

        missing_source_role = outcome_event()
        del missing_source_role["source"]["role"]
        self.assert_invalid_portable(missing_source_role)

        invalid_effective = assignment_event()
        invalid_effective["payload"]["effective"] = {
            "model": "model-a", "reasoning_effort": "medium",
            "model_observation": {
                "source": "host", "verified": False,
                "observed_at": "2026-10-05T00:00:00Z",
            },
        }
        self.assert_invalid_portable(invalid_effective)

        invalid_score = outcome_event()
        invalid_score["kind"] = "review"
        del invalid_score["payload"]["execution"]
        invalid_score["payload"]["scores"] = [{
            "rubric_version": "laya-feedback-v1", "dimension": "outcome_quality",
            "value": 3, "reason": "outside rubric", "evidence_refs": [],
            "phase": "initial", "source_sequence": 1,
            "observed_at": "2026-10-05T00:00:00Z",
        }]
        self.assert_invalid_portable(invalid_score)


if __name__ == "__main__":
    unittest.main()
