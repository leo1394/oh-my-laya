#!/usr/bin/env python3
"""Opt-in host-driven acceptance, not CI proof of a native child.

Starts an isolated deterministic decision service. The supervising host must
actually spawn the returned route and supply its native reference, unchanged
first feedback, and observed result over JSON lines. No production data is used.
EOF, timeout and errors disable collection and stop the temporary service.
"""
import argparse
import json
import importlib.util
import select
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

from laya_tell_me.advisor import preferences
from test_orchestration_transport import CURRENT_BINARY, ROOT, OrchestrationTransportTests, Service, orchestration


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def run(args):
    with tempfile.TemporaryDirectory(prefix="laya-native-dispatch-", dir=Path(tempfile.gettempdir()).resolve()) as directory:
        root = Path(directory)
        pair = {"model": args.model, "reasoning_effort": args.effort}
        preferences("auto", root / "advisor.json", ceiling=pair,
                    squad={"enabled": True, "reviewer": None})
        service = Service(CURRENT_BINARY, root, OrchestrationTransportTests().worker_copy(root))
        http = None
        try:
            http = service.authenticated_http()
            http("settings", {"recording_enabled": True}, "PATCH")
            decision = service.call(CURRENT_BINARY, {
                "state": "Read-only independent verification of the S3 efficiency evidence boundary; inspect observe() and report whether requested model and incomplete usage are kept distinct from verified effective configuration and complete task coverage. No file changes.",
                "advisor": {"models": [{"id": model, "reasoning_efforts": sorted(efforts)} for model, efforts in args.catalog.items()],
                            "current_model": args.parent_model, "current_reasoning_effort": args.parent_effort, "role": "explorer"},
                "orchestration": orchestration(run_id="native-acceptance", stage_id="evidence-audit",
                                                required_roles=["explorer"]),
            }, "native-plan")
            assert decision["orchestration_plan"]["mode"] == "delegate", decision
            route = decision["advice"]["delegation"]["spawn_parameters"]
            assert route == {"agent_type": "explorer", "model": pair["model"],
                             "reasoning_effort": pair["reasoning_effort"], "fork_turns": "none"}, decision
            decision_id = decision["meta"]["decision_id"]
            attempt = "native-evidence-audit-1"
            common = {"protocol_version": 1, "decision_id": decision_id, "attempt_ref": attempt,
                      "source": {"host": "codex", "role": "orchestrator", "actor_type": "agent"}}
            shared = {"run_id": "native-acceptance", "stage_id": "evidence-audit", "ordinal": 1,
                      "policy_version": "bounded-attempts-v1", "enforcement": "advisory"}
            observed_at = datetime.now(timezone.utc).isoformat()
            requested = {**pair, "model_observation": {"source": "spawn_request", "verified": False,
                                                       "observed_at": observed_at}}
            payload = {"recommended": None, "selected": None, "requested": requested, "effective": None,
                       "reason": "Native-host acceptance; deterministic assessment, host-authorized selected pair; effective configuration unknown",
                       "evidence_refs": []}
            events = []

            def save(event):
                assert event["decision_id"] == decision_id and event["attempt_ref"] == attempt
                reply = service.call_tool(CURRENT_BINARY, event, event["event_id"], "laya_feedback")
                assert not reply.get("isError"), reply
                ack = reply["structuredContent"]
                assert ack["status"] == "stored", ack
                events.append({"event": event, "ack": ack})
                return ack

            budget_path = ROOT / "skills/alpha-squad-coding-craft/skills/alpha-squad-coding-craft/scripts/assess_attempt_budget.py"
            spec = importlib.util.spec_from_file_location("native_acceptance_budget", budget_path)
            budget_module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(budget_module)
            gate = budget_module.assess({"events": [], "proposal": {
                "run_id": shared["run_id"], "stage_id": shared["stage_id"], "attempt_kind": "initial",
                "parent_attempt_ref": None, "change_reason": None, "evidence_refs": [], "requested": pair}})
            assert gate["eligible_for_authorization"] is True and gate["execution_authorized"] is False
            save({**common, "event_id": "native-request", "kind": "assignment", "payload": payload})
            emit({"ready": True, "provenance": "fixture assessment; native execution must come from host",
                  "decision_id": decision_id, "attempt_ref": attempt, "plan": decision["orchestration_plan"],
                  "spawn_parameters": route, "budget_gate": gate, "collection_scope": "temporary_database_only"})
            dispatch = None
            first = None
            while True:
                if not select.select([sys.stdin], [], [], 600)[0]:
                    raise TimeoutError("No host input for ten minutes")
                line = sys.stdin.readline()
                if not line:
                    break
                command = json.loads(line)
                if command["action"] == "dispatch":
                    assert dispatch is None
                    reference = command["native_execution_ref"]
                    assert isinstance(reference, str) and reference.strip()
                    dispatch = {**common, "event_id": "native-dispatch", "kind": "assignment",
                                "payload": {**payload, "execution": {**shared, "contract": "dispatch_receipt_v1",
                                    "role": "explorer", "attempt_kind": "initial", "status": "started",
                                    "native_execution_ref": reference, "context_isolation": "unknown",
                                    "context_evidence_ref": None,
                                    "input_size": {"value": None, "unit": "unknown", "source": None}}}}
                    emit(save(dispatch))
                elif command["action"] == "feedback":
                    assert dispatch is not None and first is None
                    first = command["event"]
                    assert any(score["phase"] == "initial" for score in first["payload"]["scores"])
                    emit(save(first))
                elif command["action"] == "finish":
                    assert dispatch is not None and first is not None
                    result = command["result"]
                    outcome = {**common, "event_id": "native-outcome", "kind": "outcome", "payload": {
                        "outcome": result, "summary": command["summary"], "execution": {
                            **shared, "contract": "attempt_outcome_v1", "dispatch_event_id": dispatch["event_id"],
                            "result": result, "failure_class": command.get("failure_class"),
                            "first_feedback_event_id": first["event_id"], "test_event_ids": [],
                            "review_event_ids": [first["event_id"]] if first["kind"] == "review" else [],
                            "usage_event_ids": [], "duration_ms": None}}}
                    ack = save(outcome)
                    assert save(outcome)["payload_hash"] == ack["payload_hash"]
                    detail = http("decisions/" + decision_id)
                    stored = {event["event_id"]: event for event in detail["feedback"]}
                    assert stored[first["event_id"]]["payload"] == first["payload"]
                    assert stored[dispatch["event_id"]]["payload"] == dispatch["payload"]
                    observation = detail["efficiency_observations"][0]
                    assert observation["identity"]["native_execution_ref"] == dispatch["payload"]["execution"]["native_execution_ref"]
                    assert observation["configuration"]["effective"] is None
                    assert observation["context"]["isolation"] == "unknown"
                    assert observation["usage"]["status"] == "unavailable"
                    assert observation["complete_task_coverage"] is False
                    emit({"verified": True, "events": events, "observation": observation,
                          "limitation": "host identity is caller supplied and must be verified against native transcript; no MLX or token savings measurement"})
                    break
                else:
                    raise ValueError("Unknown acceptance action")
        finally:
            try:
                if http is not None:
                    disabled = http("settings", {"recording_enabled": False}, "PATCH")
                    assert disabled["recording_enabled"] is False
            finally:
                service.close()
            emit({"collection_disabled": True, "service_stopped": service.process.poll() is not None,
                  "temporary_data_cleanup": "on context exit"})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("model", "effort", "parent-model", "parent-effort"):
        parser.add_argument("--" + name, required=True, help="Exact host-verified, already confirmed setting")
    args = parser.parse_args()
    args.catalog = {}
    for model, effort in ((args.model, args.effort), (args.parent_model, args.parent_effort)):
        args.catalog.setdefault(model, set()).add(effort)
    # macOS canonical PTYs truncate long JSON lines. Alter only this process's
    # input terminal and restore it even when the caller interrupts acceptance.
    original = None
    if sys.stdin.isatty():
        import termios
        original = termios.tcgetattr(sys.stdin.fileno())
        raw_lines = termios.tcgetattr(sys.stdin.fileno())
        raw_lines[3] &= ~(termios.ICANON | termios.ECHO)
        raw_lines[6][termios.VMIN] = 1
        raw_lines[6][termios.VTIME] = 0
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, raw_lines)
    try:
        run(args)
    finally:
        if original is not None:
            termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, original)


if __name__ == "__main__":
    main()
