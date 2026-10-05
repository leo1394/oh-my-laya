#!/usr/bin/env python3
"""Real MCP/service orchestration transport with deterministic workers only."""

import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from http.cookiejar import CookieJar


ROOT = Path(__file__).resolve().parents[1]
CURRENT_BINARY = Path(os.environ.get("LAYA_TEST_BINARY", ROOT / "target/debug/laya"))
OLD_BINARY_SOURCE = os.environ.get("LAYA_LEGACY_TEST_BINARY")
OLD_BINARY = Path(OLD_BINARY_SOURCE) if OLD_BINARY_SOURCE else None
PYTHON_BIN = Path(sys.executable).parent
TEMP_DIR = Path(tempfile.gettempdir()).resolve()
OLD_WORKER = ROOT / "tests/fixtures/workbench_worker.py"


if "-m" in sys.argv and "laya_tell_me.worker" in sys.argv:
    from laya_tell_me import server, worker

    class DeterministicTokenizer:
        mask_token = "<mask>"

        def __call__(self, text, add_special_tokens=False):
            if add_special_tokens:
                raise AssertionError("memory fixture expects raw state tokenization")
            return {"input_ids": [ord(character) for character in text]}

    class DeterministicAgent:
        cfg = {"max_len": 4096}
        tok = DeterministicTokenizer()

        def prepare(self, state, questions):
            text = state if isinstance(state, str) else json.dumps(state, ensure_ascii=False)
            state_ids = self.tok(
                text.replace(self.tok.mask_token, " "), add_special_tokens=False
            )["input_ids"]
            ids = [0] * 31 + state_ids[:self.cfg["max_len"] - 32] + [1]
            return ([{"ids": ids}] * len(questions), [{}] * len(questions))

        def predict(self, state, _questions):
            if state == "fixture:preference-gate":
                with Path(os.environ["LAYA_TEST_READY_FIFO"]).open("wb", buffering=0) as ready:
                    ready.write(b"1")
                with Path(os.environ["LAYA_TEST_RELEASE_FIFO"]).open("rb", buffering=0) as release:
                    release.read(1)
            counter = os.environ.get("LAYA_TEST_INFERENCE_COUNTER")
            if counter:
                path = Path(counter)
                count = int(path.read_text()) if path.exists() else 0
                path.write_text(str(count + 1))
            risk = "high" if "high-risk" in str(state) else "low"
            return {"answers": {
                "complexity": {"choice": "low", "confidence": 0.99},
                "risk": {"choice": risk, "confidence": 0.99},
                "certainty": {"choice": "clear", "confidence": 0.99},
            }}

    server._agent = DeterministicAgent()
    worker._loaded_agent_id = id(server._agent)
    worker._loaded_model = {
        "path": "test-only-deterministic-fixture",
        "model_revision": "test-only-verified-checkpoint",
        "checkpoint_digest": "test-only-verified-checkpoint",
        "identity_scope": "full_checkpoint_content",
        "dtype": "fixture", "device": "fixture", "batch_size": 1,
    }
    worker.main()
    raise SystemExit


def orchestration(**changes):
    value = {
        "schema_version": 1,
        "enabled": True,
        "run_id": "transport-run",
        "stage_id": "planning",
        "snapshot_revision": "snapshot-1",
        "independent_work": False,
        "dependencies_known": True,
        "required_roles": [],
        "constraint_refs": ["acceptance-scope"],
    }
    value.update(changes)
    return value


def advisor():
    return {
        "models": [{"id": "fixture-model", "reasoning_efforts": ["low", "medium"]}],
        "current_model": "fixture-model",
    }


class Service:
    def __init__(self, binary, root, worker_path, extra_env=None):
        self.root = root
        self.env = {
            **os.environ,
            "PATH": str(PYTHON_BIN) + os.pathsep + os.environ.get("PATH", ""),
            "PYTHONPATH": str(ROOT / "src"),
            "PYTHONDONTWRITEBYTECODE": "1",
            "LAYA_WORKBENCH_DIR": str(root),
            "LAYA_PORT": "0",
            "LAYA_PYTHON": str(worker_path),
            "LAYA_ADVISOR_CONFIG": str(root / "advisor.json"),
            **(extra_env or {}),
        }
        self.process = subprocess.Popen(
            [str(binary), "service"], env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        for _ in range(250):
            try:
                info = json.loads((root / "service.json").read_text())
                if info["pid"] == self.process.pid:
                    with socket.socket(socket.AF_UNIX) as probe:
                        probe.settimeout(0.1)
                        probe.connect(str(root / "service.sock"))
                    break
            except (FileNotFoundError, KeyError, ValueError, OSError):
                pass
            if self.process.poll() is not None:
                error = self.process.stderr.read().decode()
                self.process.stdout.close()
                self.process.stderr.close()
                raise AssertionError(error)
            time.sleep(0.02)
        else:
            self.close()
            raise TimeoutError("service did not become ready")

    def rpc(self, method, params=None, request_id="transport-status"):
        with socket.socket(socket.AF_UNIX) as channel:
            channel.settimeout(10)
            channel.connect(str(self.root / "service.sock"))
            channel.sendall(json.dumps({
                "protocol_version": 1,
                "request_id": request_id,
                "method": method,
                "params": params or {},
            }).encode() + b"\n")
            reply = json.loads(channel.makefile("rb").readline())
        if "error" in reply:
            raise RuntimeError(reply["error"])
        return reply["result"]

    def call_tool(self, bridge, arguments, request_id, tool_name="laya_tell_me"):
        request = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": arguments},
        }
        completed = subprocess.run(
            [str(bridge), "mcp"], env=self.env,
            input=json.dumps(request) + "\n", text=True,
            capture_output=True, timeout=30, check=True,
        )
        response = json.loads(completed.stdout)
        return response["result"]

    def call(self, bridge, arguments, request_id):
        tool = self.call_tool(bridge, arguments, request_id)
        if tool.get("isError"):
            raise AssertionError(tool)
        return tool["structuredContent"]

    def authenticated_http(self):
        url = urllib.parse.urlparse(self.rpc("pair")["url"])
        origin = f"{url.scheme}://{url.netloc}"
        opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(CookieJar())
        )

        def request(path, body=None, method=None):
            headers = {"Origin": origin}
            if body is not None:
                headers["Content-Type"] = "application/json"
            call = urllib.request.Request(
                origin + "/api/v1/" + path,
                json.dumps(body).encode() if body is not None else None,
                headers=headers, method=method,
            )
            with opener.open(call, timeout=5) as response:
                return json.loads(response.read())

        code = urllib.parse.parse_qs(url.fragment)["pair"][0]
        if request("pair", {"code": code}, "POST") != {"paired": True}:
            raise AssertionError("temporary service pairing failed")
        return request

    def close(self):
        if self.process.poll() is None:
            try:
                self.rpc("stop")
            except Exception:
                self.process.terminate()
        self.process.wait(timeout=5)
        self.process.stdout.close()
        self.process.stderr.close()


@unittest.skipUnless(CURRENT_BINARY.exists(), "build the current Rust service first")
class OrchestrationTransportTests(unittest.TestCase):
    def legacy_worker_copy(self, root):
        path = root / "legacy-python"
        original = OLD_WORKER.read_text()
        marker = '    elif method == "predict":\n'
        self.assertIn(marker, original)
        path.write_text(original.replace(marker, marker +
            '        assert "orchestration" not in request["params"], "legacy worker rejects unknown parameters"\n'))
        path.chmod(0o700)
        rejected = subprocess.run([str(sys.executable), str(path)], input=json.dumps({
            "protocol_version": 1, "request_id": "negative-control", "method": "predict",
            "params": {"state": "test", "orchestration": None},
        }) + "\n", text=True, capture_output=True, timeout=5)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("legacy worker rejects unknown parameters", rejected.stderr)
        return path

    def worker_copy(self, root):
        path = root / "deterministic-python"
        shutil.copy2(__file__, path)
        path.chmod(0o700)
        return path

    def test_current_transport_is_opt_in_and_binds_the_decision(self):
        with tempfile.TemporaryDirectory(dir=TEMP_DIR, prefix="laya-orchestration-") as directory:
            root = Path(directory)
            service = Service(CURRENT_BINARY, root, self.worker_copy(root))
            try:
                self.assertFalse(service.rpc("status")["settings"]["recording_enabled"])
                enabled = service.call(CURRENT_BINARY, {
                    "state": "small documentation correction",
                    "advisor": advisor(),
                    "orchestration": orchestration(),
                }, "enabled")
                plan = enabled["orchestration_plan"]
                self.assertEqual(plan["mode"], "direct")
                self.assertEqual(plan["decision_id"], enabled["meta"]["decision_id"])
                self.assertFalse(plan["execution_authorized"])
                self.assertFalse(plan["parent_model_switched"])
                self.assertEqual(plan["enforcement"], "advisory")
                self.assertEqual(enabled["meta"]["recording_status"], "not_recorded")
                receipt = enabled["meta"]["memory_receipt"]
                self.assertEqual(receipt["contract"], "memory_receipt_v1")
                self.assertEqual(receipt["selected_case_ids"], [])
                self.assertEqual(receipt["received_case_ids"], [])
                self.assertTrue(receipt["complete_base_state"])

                legacy = service.call(CURRENT_BINARY, {
                    "state": "small documentation correction", "advisor": advisor(),
                }, "legacy")
                disabled = service.call(CURRENT_BINARY, {
                    "state": "small documentation correction",
                    "advisor": advisor(),
                    "orchestration": orchestration(enabled=False),
                }, "disabled")
                for result in (legacy, disabled):
                    self.assertNotIn("orchestration_plan", result)
                    self.assertNotIn("orchestration_status", result["meta"])
                    self.assertNotIn("memory_receipt", result["meta"])
                self.assertEqual(set(legacy), set(disabled))
                self.assertEqual(legacy["laya_result"], disabled["laya_result"])
                self.assertEqual(legacy["advice"], disabled["advice"])
                self.assertEqual(set(legacy["meta"]), set(disabled["meta"]))
                null = service.call(CURRENT_BINARY, {
                    "state": "small documentation correction",
                    "advisor": advisor(), "orchestration": None,
                }, "null")
                self.assertEqual(set(legacy), set(null))
                self.assertNotIn("orchestration_status", null["meta"])

                malformed = service.call_tool(CURRENT_BINARY, {
                    "state": "small documentation correction",
                    "advisor": advisor(), "orchestration": {"enabled": False},
                }, "malformed-disabled")
                self.assertTrue(malformed["isError"])
                self.assertIn("versioned context fields", malformed["content"][0]["text"])
                private = service.call_tool(CURRENT_BINARY, {
                    "state": "small documentation correction", "advisor": advisor(),
                    "orchestration": orchestration(), "_cache_context": {},
                }, "caller-cache-context")
                self.assertTrue(private["isError"])
                self.assertIn("service-owned", private["content"][0]["text"])
                private_budget = service.call_tool(CURRENT_BINARY, {
                    "state": "small documentation correction", "advisor": advisor(),
                    "orchestration": orchestration(), "memory_budget": True,
                }, "caller-memory-budget")
                self.assertTrue(private_budget["isError"])
                self.assertIn("service-owned", private_budget["content"][0]["text"])

                risky = service.call(CURRENT_BINARY, {
                    "state": "high-risk migration review",
                    "advisor": advisor(),
                    "orchestration": orchestration(),
                }, "high-risk")
                self.assertEqual(risky["orchestration_plan"]["mode"], "delegate")
                self.assertEqual(risky["orchestration_plan"]["required_roles"], ["reviewer"])
                self.assertIn("high_risk_requires_review", risky["orchestration_plan"]["reason_codes"])
                self.assertEqual(service.rpc("status")["counts"]["decisions"], 0)

                http = service.authenticated_http()
                self.assertTrue(http("settings", {"recording_enabled": True}, "PATCH")["recording_enabled"])
                try:
                    persisted = service.call(CURRENT_BINARY, {
                        "state": "persisted temporary planning response",
                        "advisor": advisor(),
                        "orchestration": orchestration(),
                    }, "persisted")
                    decision_id = persisted["meta"]["decision_id"]
                    self.assertEqual(persisted["meta"]["recording_status"], "stored")
                    detail = http("decisions/" + decision_id)
                    self.assertEqual(detail["result"]["orchestration_plan"],
                                     persisted["orchestration_plan"])
                    self.assertEqual(detail["result"]["orchestration_plan"]["decision_id"],
                                     decision_id)
                finally:
                    disabled_settings = http("settings", {"recording_enabled": False}, "PATCH")
                self.assertFalse(disabled_settings["recording_enabled"])
            finally:
                service.close()

    def test_concurrent_roles_reuse_and_mutations_invalidate(self):
        with tempfile.TemporaryDirectory(dir=TEMP_DIR, prefix="laya-cache-transport-") as directory:
            root = Path(directory)
            counter = root / "inference-count"
            service = Service(CURRENT_BINARY, root, self.worker_copy(root), {
                "LAYA_TEST_INFERENCE_COUNTER": str(counter),
            })
            context = orchestration(required_roles=["worker", "reviewer"])

            def routed(role, request_id):
                catalog = advisor()
                catalog.update({
                    "role": role, "current_reasoning_effort": "medium",
                    "execution_choice": {"model": "fixture-model", "reasoning_effort": "medium"},
                })
                return service.call(CURRENT_BINARY, {
                    "state": "shared assessment", "advisor": catalog,
                    "orchestration": context,
                }, request_id)

            try:
                with ThreadPoolExecutor(max_workers=2) as clients:
                    first, second = list(clients.map(
                        lambda value: routed(*value),
                        (("worker", "concurrent-worker"), ("reviewer", "concurrent-reviewer")),
                    ))
                self.assertEqual(counter.read_text(), "1")
                self.assertEqual(sorted([
                    first["meta"]["assessment_cache"]["status"],
                    second["meta"]["assessment_cache"]["status"],
                ]), ["hit", "miss"])
                hit = first if first["meta"]["assessment_cache"]["status"] == "hit" else second
                miss = second if hit is first else first
                self.assertEqual(hit["meta"]["reused_from_decision_id"],
                                 miss["meta"]["decision_id"])
                self.assertEqual(hit["meta"]["assessment_cache"]["inference_input_tokens"], 0)
                self.assertEqual(hit["meta"]["assessment_cache"]["inference_output_tokens"], 0)

                http = service.authenticated_http()
                self.assertTrue(http("settings", {"recording_enabled": True}, "PATCH")["recording_enabled"])
                try:
                    after_settings = routed("worker", "after-settings")
                    self.assertEqual(counter.read_text(), "2")
                    self.assertEqual(after_settings["meta"]["assessment_cache"]["status"], "miss")
                    decision_id = after_settings["meta"]["decision_id"]
                    self.assertEqual(after_settings["meta"]["recording_status"], "stored")
                    http("decisions/" + decision_id, method="DELETE")
                    after_delete = routed("reviewer", "after-delete")
                    self.assertEqual(counter.read_text(), "3")
                    self.assertEqual(after_delete["meta"]["assessment_cache"]["status"], "miss")
                    event = {
                        "protocol_version": 1, "event_id": "cache-feedback",
                        "decision_id": after_delete["meta"]["decision_id"],
                        "attempt_ref": "cache-attempt", "kind": "test",
                        "source": {"host": "test", "role": "tester", "actor_type": "agent"},
                        "payload": {"result": "pass", "summary": "deterministic cache fixture"},
                    }
                    first_feedback = service.rpc("feedback", event, "feedback-first")
                    self.assertEqual(first_feedback["status"], "stored")
                    after_feedback = routed("worker", "after-feedback")
                    self.assertEqual(counter.read_text(), "4")
                    self.assertEqual(after_feedback["meta"]["assessment_cache"]["status"], "miss")
                    replay = service.rpc("feedback", event, "feedback-replay")
                    self.assertEqual(replay["status"], "stored")
                    self.assertTrue(replay["idempotent"])
                    after_replay = routed("reviewer", "after-replay")
                    self.assertEqual(counter.read_text(), "4")
                    self.assertEqual(after_replay["meta"]["assessment_cache"]["status"], "hit")
                finally:
                    http("settings", {"recording_enabled": False}, "PATCH")
            finally:
                service.close()

    def test_execution_receipts_round_trip_through_feedback_bridge(self):
        with tempfile.TemporaryDirectory(dir=TEMP_DIR, prefix="laya-execution-transport-") as directory:
            root = Path(directory)
            service = Service(CURRENT_BINARY, root, self.worker_copy(root))
            http = service.authenticated_http()
            self.assertTrue(http("settings", {"recording_enabled": True}, "PATCH")["recording_enabled"])
            try:
                decision = service.call(CURRENT_BINARY, {
                    "state": "temporary execution receipt transport",
                    "advisor": advisor(), "orchestration": orchestration(),
                }, "execution-decision")
                decision_id = decision["meta"]["decision_id"]
                self.assertEqual(decision["meta"]["recording_status"], "stored")
                attempt_ref = "transport-attempt-1"
                execution = {
                    "contract": "dispatch_receipt_v1", "run_id": "transport-run",
                    "stage_id": "planning", "ordinal": 1,
                    "policy_version": "bounded-attempts-v1", "enforcement": "advisory",
                    "role": "worker", "attempt_kind": "initial", "status": "started",
                    "native_execution_ref": "test-only:native-segment-1",
                    "context_isolation": "isolated",
                    "context_evidence_ref": "test-only:context-snapshot-1",
                    "input_size": {"value": None, "unit": "unknown", "source": None},
                }
                assignment = {
                    "protocol_version": 1, "event_id": "transport-dispatch",
                    "decision_id": decision_id, "attempt_ref": attempt_ref,
                    "kind": "assignment",
                    "source": {"host": "codex", "role": "orchestrator", "actor_type": "agent"},
                    "payload": {
                        "recommended": None, "selected": None, "requested": None,
                        "effective": None, "reason": "test-only native protocol transport",
                        "evidence_refs": [], "execution": execution,
                    },
                }
                assignment_tool = service.call_tool(
                    CURRENT_BINARY, assignment, "execution-assignment", "laya_feedback"
                )
                self.assertFalse(assignment_tool.get("isError"), assignment_tool)
                assignment_ack = assignment_tool["structuredContent"]
                self.assertEqual(assignment_ack["status"], "stored")
                self.assertEqual(assignment_ack["event_id"], assignment["event_id"])
                self.assertEqual(len(assignment_ack["payload_hash"]), 64)

                outcome_execution = {
                    "contract": "attempt_outcome_v1", "run_id": "transport-run",
                    "stage_id": "planning", "ordinal": 1,
                    "policy_version": "bounded-attempts-v1", "enforcement": "advisory",
                    "dispatch_event_id": assignment["event_id"], "result": "failure",
                    "failure_class": "acceptance", "first_feedback_event_id": None,
                    "test_event_ids": [], "review_event_ids": [], "usage_event_ids": [],
                    "duration_ms": 25,
                }
                outcome = {
                    "protocol_version": 1, "event_id": "transport-outcome",
                    "decision_id": decision_id, "attempt_ref": attempt_ref,
                    "kind": "outcome",
                    "source": {"host": "codex", "role": "orchestrator", "actor_type": "agent"},
                    "payload": {
                        "outcome": "failure", "summary": "test-only transport outcome",
                        "execution": outcome_execution,
                    },
                }
                outcome_tool = service.call_tool(
                    CURRENT_BINARY, outcome, "execution-outcome", "laya_feedback"
                )
                self.assertFalse(outcome_tool.get("isError"), outcome_tool)
                outcome_ack = outcome_tool["structuredContent"]
                self.assertEqual(outcome_ack["status"], "stored")
                self.assertEqual(outcome_ack["event_id"], outcome["event_id"])
                self.assertEqual(len(outcome_ack["payload_hash"]), 64)
                replay = service.call_tool(
                    CURRENT_BINARY, outcome, "execution-outcome-replay", "laya_feedback"
                )["structuredContent"]
                self.assertEqual(replay["status"], "stored")
                self.assertEqual(replay["payload_hash"], outcome_ack["payload_hash"])

                detail = http("decisions/" + decision_id)
                stored = {item["event_id"]: item for item in detail["feedback"]}
                self.assertEqual(stored[assignment["event_id"]]["payload_hash"],
                                 assignment_ack["payload_hash"])
                self.assertEqual(stored[outcome["event_id"]]["payload_hash"],
                                 outcome_ack["payload_hash"])
                self.assertEqual(stored[assignment["event_id"]]["source"]["role"], "orchestrator")
                self.assertEqual(stored[assignment["event_id"]]["payload"]["execution"], execution)
                self.assertEqual(stored[outcome["event_id"]]["payload"]["execution"],
                                 outcome_execution)
                self.assertEqual(detail["execution_attempts"][0]["role"], "orchestrator")
            finally:
                disabled = http("settings", {"recording_enabled": False}, "PATCH")
                self.assertFalse(disabled["recording_enabled"])
                service.close()

    def test_async_evaluation_preserves_v3_control_and_emits_opt_in_v4_receipts(self):
        with tempfile.TemporaryDirectory(dir=TEMP_DIR, prefix="leval-") as directory:
            root = Path(directory)
            service = Service(CURRENT_BINARY, root, self.worker_copy(root))
            http = service.authenticated_http()
            self.assertTrue(http("settings", {"recording_enabled": True}, "PATCH")["recording_enabled"])

            def wait_job(job):
                for _ in range(500):
                    job = http("jobs/" + job["id"])
                    if job["status"] in ("completed", "failed", "cancelled"):
                        return job
                    time.sleep(0.02)
                self.fail("temporary evaluation job did not finish")

            try:
                decision = service.call(CURRENT_BINARY, {
                    "state": "Correct a README spelling error without code changes.",
                    "advisor": advisor(),
                }, "evaluation-seed")
                decision_id = decision["meta"]["decision_id"]
                review = http("decisions/" + decision_id + "/reviews", {
                    "expected_revision": 0, "status": "confirmed",
                    "labels": {"complexity": "low", "risk": "low", "certainty": "clear"},
                    "task_family": "documentation", "task_lineage": "transport-reviewed-readme",
                    "language": "en", "applicability": "task-fact",
                    "reason": "deterministic transport fixture",
                })
                legacy_version = http("memory-versions", {"case_ids": [review["case_id"]]})
                legacy_job = wait_job(http("jobs", {
                    "kind": "evaluation", "version_id": legacy_version["id"],
                }))
                self.assertEqual(legacy_job["status"], "completed", legacy_job)
                legacy_report = legacy_job["result"]
                self.assertEqual(legacy_report["evaluator_identity"], "laya-advisor-evaluator-v3")
                self.assertNotIn("input_policy", legacy_report)
                self.assertTrue(all(
                    run["memory_budget_required"] is False
                    for row in legacy_report["rows"] for run in row["runs"]
                ))
                self.assertTrue(all(
                    "memory_receipt" not in run["result"]["meta"]
                    for row in legacy_report["rows"] for run in row["runs"]
                ))
                self.assertGreater(legacy_report["candidate_memory_exposure"], 0)
                http("memory-versions/" + legacy_version["id"] + "/activate", {})

                candidate = http("memory-versions", {"case_ids": [review["case_id"]]})
                malformed = wait_job(http("jobs", {
                    "kind": "evaluation", "version_id": candidate["id"],
                    "memory_budget": "true",
                }))
                self.assertEqual(malformed["status"], "failed")
                self.assertIn("memory_budget must be boolean", malformed["error"])

                opt_in = wait_job(http("jobs", {
                    "kind": "evaluation", "version_id": candidate["id"],
                    "memory_budget": True,
                }))
                self.assertEqual(opt_in["status"], "completed", opt_in)
                report = opt_in["result"]
                self.assertEqual(report["evaluator_identity"], "laya-advisor-evaluator-v4")
                self.assertEqual(report["input_policy"], "whole-case-token-budget-v1")
                self.assertEqual(report["current_version"], legacy_version["id"])
                for row in report["rows"]:
                    current, evaluated = row["runs"][1], row["runs"][2]
                    self.assertFalse(current["memory_budget_required"])
                    self.assertNotIn("memory_receipt", current["result"]["meta"])
                    self.assertTrue(evaluated["memory_budget_required"])
                    receipt = evaluated["result"]["meta"]["memory_receipt"]
                    self.assertEqual(receipt["contract"], "memory_receipt_v1")
                    self.assertEqual(receipt["policy_version"], "whole-case-token-budget-v1")
                    self.assertEqual(receipt["usage_scope"], "current_evaluation")
                    self.assertEqual(receipt["serialization"],
                                     "laya-state-mask-normalized-utf8")
                    self.assertIsInstance(receipt["base_state_bytes"], int)
                    self.assertIsInstance(receipt["packed_state_bytes"], int)
                    self.assertEqual(receipt["selected_case_ids"], evaluated["case_ids"])
                    self.assertEqual(receipt["received_case_ids"],
                                     evaluated["result"]["meta"]["case_ids"])
            finally:
                disabled = http("settings", {"recording_enabled": False}, "PATCH")
                self.assertFalse(disabled["recording_enabled"])
                service.close()

    def test_queued_preferences_do_not_block_feedback_persistence(self):
        with tempfile.TemporaryDirectory(dir=TEMP_DIR, prefix="laya-preferences-queue-") as directory:
            root = Path(directory)
            ready = root / "ready.fifo"
            release = root / "release.fifo"
            os.mkfifo(ready)
            os.mkfifo(release)
            service = Service(CURRENT_BINARY, root, self.worker_copy(root), {
                "LAYA_TEST_READY_FIFO": str(ready),
                "LAYA_TEST_RELEASE_FIFO": str(release),
            })
            http = service.authenticated_http()
            self.assertTrue(http("settings", {"recording_enabled": True}, "PATCH")["recording_enabled"])
            released = False
            try:
                slow_params = {
                    "state": "fixture:preference-gate", "advisor": advisor(),
                    "orchestration": orchestration(),
                }
                with ThreadPoolExecutor(max_workers=2) as clients:
                    slow = clients.submit(service.rpc, "predict", slow_params, "queued-decision")
                    with ready.open("rb", buffering=0) as signal:
                        self.assertEqual(signal.read(1), b"1")
                    preferences = clients.submit(
                        service.rpc, "preferences", {"policy": "conditional"}, "queued-preferences"
                    )
                    for _ in range(100):
                        if service.rpc("status")["worker"]["queued"] == 1:
                            break
                    self.assertEqual(service.rpc("status")["worker"]["queued"], 1)
                    event = {
                        "protocol_version": 1, "event_id": "queued-feedback",
                        "decision_id": "queued-decision", "attempt_ref": "queued-attempt",
                        "kind": "test",
                        "source": {"host": "test", "role": "tester", "actor_type": "agent"},
                        "payload": {"result": "pass", "summary": "persists while preferences waits"},
                    }
                    feedback = service.rpc("feedback", event, "queued-feedback-request")
                    self.assertEqual(feedback["status"], "stored")
                    self.assertFalse(preferences.done())
                    self.assertFalse(slow.done())
                    with release.open("wb", buffering=0) as signal:
                        signal.write(b"1")
                    released = True
                    self.assertIn("policy", preferences.result(timeout=10))
                    self.assertEqual(slow.result(timeout=10)["meta"]["decision_id"], "queued-decision")
                detail = http("decisions/queued-decision")
                self.assertEqual({item["event_id"] for item in detail["feedback"]}, {"queued-feedback"})
            finally:
                if not released and service.rpc("status")["worker"]["busy"]:
                    with release.open("wb", buffering=0) as signal:
                        signal.write(b"1")
                http("settings", {"recording_enabled": False}, "PATCH")
                service.close()

    def test_current_service_marks_an_old_worker_unsupported(self):
        with tempfile.TemporaryDirectory(dir=TEMP_DIR, prefix="laya-old-worker-") as directory:
            service = Service(CURRENT_BINARY, Path(directory), self.legacy_worker_copy(Path(directory)))
            try:
                result = service.call(CURRENT_BINARY, {
                    "state": "legacy worker",
                    "advisor": advisor(),
                    "orchestration": orchestration(),
                }, "old-worker")
                self.assertNotIn("orchestration_plan", result)
                self.assertEqual(result["meta"]["orchestration_status"], "unsupported_worker")
                for label, context in (("disabled", orchestration(enabled=False)), ("null", None)):
                    ordinary = service.call(CURRENT_BINARY, {
                        "state": "legacy worker", "advisor": advisor(),
                        "orchestration": context,
                    }, "old-worker-" + label)
                    self.assertNotIn("orchestration_plan", ordinary)
                    self.assertNotIn("orchestration_status", ordinary["meta"])
            finally:
                service.close()

    @unittest.skipUnless(OLD_BINARY is not None and OLD_BINARY.exists(),
                         "set LAYA_LEGACY_TEST_BINARY to a published pre-contract binary")
    def test_current_bridge_marks_an_old_service_unsupported(self):
        with tempfile.TemporaryDirectory(dir=TEMP_DIR, prefix="laya-old-service-") as directory:
            service = Service(OLD_BINARY, Path(directory), self.legacy_worker_copy(Path(directory)))
            try:
                result = service.call(CURRENT_BINARY, {
                    "state": "legacy service",
                    "advisor": advisor(),
                    "orchestration": orchestration(),
                }, "old-service")
                self.assertNotIn("orchestration_plan", result)
                self.assertEqual(result["meta"]["orchestration_status"], "unsupported_service")
                for label, context in (("disabled", orchestration(enabled=False)), ("null", None)):
                    ordinary = service.call(CURRENT_BINARY, {
                        "state": "legacy service", "advisor": advisor(),
                        "orchestration": context,
                    }, "old-service-" + label)
                    self.assertNotIn("orchestration_plan", ordinary)
                    self.assertNotIn("orchestration_status", ordinary["meta"])
            finally:
                service.close()


if __name__ == "__main__":
    unittest.main()
