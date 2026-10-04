"""Scenario estimate acceptance through the real isolated service (synthetic inputs)."""
import tempfile
import unittest
import json
import urllib.parse
import urllib.request
from http.cookiejar import CookieJar
from pathlib import Path

from test_service_failures import BINARY, Service


@unittest.skipUnless(BINARY.exists(), "Build the Rust service first")
class ScenarioEstimateTests(unittest.TestCase):
    def test_one_run_without_baseline_survives_restart_and_privacy_delete(self):
        with tempfile.TemporaryDirectory(prefix="laya-scenario-", dir="/private/tmp") as directory:
            root = Path(directory)
            service = Service(root)
            try:
                service.consent()
                decision = service.rpc("predict", {"state": "Synthetic scenario task", "advisor": {"models": []}}, "scenario-decision")
                decision_id = decision["meta"]["decision_id"]
                source = {"host": "test", "role": "orchestrator", "actor_type": "agent"}
                usage = {
                    "protocol_version": 1, "event_id": "scenario-usage", "decision_id": decision_id,
                    "attempt_ref": "execution", "kind": "usage", "source": source,
                    "payload": {"total_tokens": 200, "source": "synthetic-native-fixture", "source_verified": True,
                                "scope": "attempt", "checkpoint": "final", "overlap_status": "non_overlapping",
                                "aggregation": "cumulative", "usage_stream_id": "fixture-stream", "source_sequence": 1},
                }
                self.assertEqual(service.rpc("feedback", usage)["status"], "stored")
                manifest = {
                    "protocol_version": 1, "event_id": "scenario-manifest", "decision_id": decision_id,
                    "attempt_ref": "execution", "kind": "run_manifest", "source": source,
                    "payload": {
                        "manifest_version": 1, "run_id": "fixture-run", "host_root_ref": "fixture-root",
                        "mode": "laya", "observed_at": "2026-10-04T00:00:00Z", "terminal_checkpoint": "final",
                        "context": {key: None for key in ("task_snapshot_hash", "code_revision", "test_snapshot_hash",
                                                         "tool_environment_id", "external_inputs_hash", "acceptance_policy")},
                        "meter": {"unit": "tokens", "identity": "fixture-token-meter", "scope": "host_only",
                                  "excluded_components": ["local_laya_inference"]},
                        "segments": [{"decision_id": decision_id, "attempt_ref": "execution", "usage_event_id": "scenario-usage"}],
                        "outcome_event_ids": [], "coverage": {"status": "partial", "evidence_refs": ["fixture-ledger"]},
                        "scenario": {"orchestrator_model": "fixture-model", "reasoning_effort": "medium",
                                     "initial_context_tokens": 100, "input_source": "synthetic heuristic inputs, not observed savings",
                                     "stages": [{"context_growth_tokens": 50, "work_output_tokens": 20, "passes": 2},
                                                {"context_growth_tokens": 30, "work_output_tokens": 10, "passes": 1}]},
                    },
                }
                first = service.rpc("feedback", manifest)
                self.assertEqual(first["status"], "stored")
                self.assertTrue(service.rpc("feedback", manifest)["idempotent"])
                tokens = service.rpc("status")["dashboard"]["tokens"]
                self.assertEqual(tokens["recorded_total"], 200)
                self.assertEqual(tokens["scenario"]["included_runs"], 1)
                self.assertEqual(tokens["scenario"]["actual_total"], 200)
                self.assertEqual(tokens["scenario"]["saved"]["central"], 295)
                self.assertEqual(tokens["scenario"]["baseline"]["central"], 495)
                self.assertEqual(tokens["scenario"]["scope"], "host_only")
                self.assertEqual(tokens["scenario"]["runs"][0]["orchestrator_model"], "fixture-model")
                self.assertEqual(tokens["scenario"]["runs"][0]["reasoning_effort"], "medium")
                service.close()
                service = Service(root)
                self.assertEqual(service.rpc("status")["dashboard"]["tokens"]["scenario"], tokens["scenario"])
                url = urllib.parse.urlparse(service.rpc("pair")["url"])
                origin = f"{url.scheme}://{url.netloc}"
                opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))
                for path, body, method in (("pair", {"code": urllib.parse.parse_qs(url.fragment)["pair"][0]}, "POST"),
                                           (f"decisions/{decision_id}", {}, "DELETE")):
                    request = urllib.request.Request(origin + "/api/v1/" + path, json.dumps(body).encode(),
                        headers={"Origin": origin, "Content-Type": "application/json"}, method=method)
                    with opener.open(request, timeout=5) as response:
                        response.read()
                after = service.rpc("status")["dashboard"]["tokens"]
                self.assertIsNone(after["scenario"]["saved"])
                self.assertEqual(after["scenario"]["included_runs"], 0)
            finally:
                service.close()
