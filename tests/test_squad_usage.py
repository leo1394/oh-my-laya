"""Opt-in companion producer -> real service acceptance with synthetic native logs."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

from test_service_failures import BINARY, Service


SQUAD_SOURCE = os.environ.get("LAYA_SQUAD_SOURCE")


@unittest.skipUnless(SQUAD_SOURCE and BINARY.exists(), "Set LAYA_SQUAD_SOURCE and build the Rust service")
class SquadUsageTests(unittest.TestCase):
    def test_exclusive_native_segment_survives_replay_reordering_and_restart(self):
        scripts = Path(SQUAD_SOURCE) / "skills/alpha-squad-coding-craft/scripts"
        with tempfile.TemporaryDirectory(prefix="laya-squad-usage-", dir="/private/tmp") as directory:
            root = Path(directory)
            sessions = root / "codex/sessions"
            sessions.mkdir(parents=True)

            def record(thread, response, total, cumulative):
                return {"type": "token_usage_record", "timestamp": "2026-10-04T00:00:00Z", "payload": {
                    "thread_id": thread, "turn_id": f"{thread}-turn", "root_turn_id": "test-root",
                    "response_id": response, "usage": {"total_tokens": total},
                    "turn_token_usage": {"total_tokens": cumulative},
                }}

            def log(thread, entries, parent=None):
                metadata = {"type": "session_meta", "payload": {"id": thread, "source": {
                    "subagent": {"thread_spawn": {"parent_thread_id": parent}},
                }}}
                (sessions / f"{thread}.jsonl").write_text(
                    "".join(json.dumps(item) + "\n" for item in [metadata, *entries]))

            def run(script, *args):
                result = subprocess.run([sys.executable, str(scripts / script), *args],
                                        capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return json.loads(result.stdout)

            def prepare():
                report = run("collect_token_usage.py", "--codex-home", str(root / "codex"),
                             "--root-thread", "parent", "--root-turn", "test-root", "--expected-subagents", "1")
                (root / "usage.json").write_text(json.dumps(report))
                prepared = run("prepare_laya_usage.py", "--report", str(root / "usage.json"),
                               "--bindings", str(root / "bindings.json"))
                self.assertEqual(prepared["status"], "ok")
                self.assertEqual(prepared["unbound_segments"], 1)
                self.assertEqual(len(prepared["events"]), 1)
                return prepared["events"][0]

            log("parent", [record("parent", "p1", 10, 10)])
            log("child", [record("child", "c1", 20, 20)], "parent")
            service = Service(root / "runtime")
            try:
                self.assertFalse(service.rpc("status")["settings"]["recording_enabled"])
                service.consent()  # Disposable test database only; never the real user database.
                decision = service.rpc("predict", {"state": "Synthetic usage acceptance", "advisor": {"models": []}}, "usage-decision")
                decision_id = decision["meta"]["decision_id"]
                (root / "bindings.json").write_text(json.dumps([{
                    "thread_id": "child", "turn_id": "child-turn", "root_turn_id": "test-root",
                    "decision_id": decision_id, "attempt_ref": "child-execution",
                    "configuration_scope_confirmed": True,
                }]))
                first = prepare()
                self.assertEqual(first["payload"]["total_tokens"], 20)
                self.assertEqual(prepare(), first)
                log("child", [record("child", "c1", 20, 20), record("child", "c2", 30, 50)], "parent")
                latest = prepare()
                self.assertEqual(latest["payload"]["total_tokens"], 50)
                self.assertNotEqual(latest["event_id"], first["event_id"])
                self.assertEqual(latest["payload"]["usage_stream_id"], first["payload"]["usage_stream_id"])
                for event in (latest, first, latest):
                    self.assertEqual(service.rpc("feedback", event, event["event_id"])["status"], "stored")
                tokens = service.rpc("status")["dashboard"]["tokens"]
                self.assertEqual(tokens["recorded_total"], 50)  # Parent's inclusive task total is not re-added.
                self.assertEqual(tokens["included_attempts"], 1)
                (root / "scenario-plan.json").write_text(json.dumps({
                    "run_id": "fixture-scenario", "observed_at": "2026-10-04T00:00:00Z",
                    "orchestrator": {"model": "fixture-model", "reasoning_effort": "medium",
                                     "source": "host", "reference": "fixture-turn-context"},
                    "meter_identity": "fixture-native-meter", "initial_context_text": "a" * 20,
                    "stages": [{"thread_id": "child", "turn_id": "child-turn", "root_turn_id": "test-root",
                                "context_text": "b" * 8, "work_output_text": "c" * 4, "passes": 2}],
                }))
                scenario = run("prepare_laya_scenario.py", "--report", str(root / "usage.json"),
                               "--bindings", str(root / "bindings.json"), "--plan", str(root / "scenario-plan.json"))
                self.assertEqual(scenario["status"], "ok")
                self.assertEqual(scenario["unbound_segments"], 1)
                self.assertEqual([event["kind"] for event in scenario["events"]], ["usage", "run_manifest"])
                for event in scenario["events"]:
                    self.assertEqual(service.rpc("feedback", event, event["event_id"])["status"], "stored")
                estimated = service.rpc("status")["dashboard"]["tokens"]["scenario"]
                self.assertEqual(estimated["actual_total"], 50)
                self.assertEqual(estimated["baseline"]["central"], 15)
                self.assertEqual(estimated["saved"]["central"], -35)
                self.assertEqual(estimated["status"], "partial")
                with sqlite3.connect(root / "runtime/laya.sqlite3") as db:
                    self.assertEqual(db.execute("SELECT count(*) FROM feedback_events WHERE kind='usage'").fetchone()[0], 2)
                service.close()
                service = Service(root / "runtime")
                self.assertEqual(service.rpc("status")["dashboard"]["tokens"]["recorded_total"], 50)
                self.assertEqual(service.rpc("status")["dashboard"]["tokens"]["scenario"], estimated)
            finally:
                service.close()


if __name__ == "__main__":
    unittest.main()
