"""Exercise real Rust transports/SQLite/UI auth in a disposable data directory."""
from http.cookiejar import CookieJar
from concurrent.futures import ThreadPoolExecutor
import json
import copy
import fcntl
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("LAYA_TEST_BINARY", str(ROOT / "target/debug/laya"))).resolve()
FIXTURE = ROOT / "tests/fixtures/workbench_worker.py"


def main():
    if not BINARY.is_file():
        raise FileNotFoundError(f"LAYA_TEST_BINARY must name an existing regular file: {BINARY}")
    FIXTURE.chmod(0o755)
    with tempfile.TemporaryDirectory(prefix="laya-smoke-", dir="/private/tmp") as directory:
        data = Path(directory)
        env = {**os.environ, "LAYA_WORKBENCH_DIR": str(data), "LAYA_PORT": "0", "LAYA_PYTHON": str(FIXTURE), "LAYA_IDLE_SECONDS": "1"}
        service = subprocess.Popen([str(BINARY), "service"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

        def rpc(method, params=None, request_id=None):
            request = {"protocol_version": 1, "request_id": request_id or f"smoke-{time.time_ns()}", "method": method, "params": params or {}}
            with socket.socket(socket.AF_UNIX) as channel:
                channel.settimeout(10)
                channel.connect(str(data / "service.sock"))
                channel.sendall(json.dumps(request).encode() + b"\n")
                response = json.loads(channel.makefile("rb").readline())
            if "error" in response:
                raise RuntimeError(response["error"])
            return response["result"]

        try:
            for _ in range(100):
                if (data / "service.json").exists():
                    break
                if service.poll() is not None:
                    raise AssertionError(service.stderr.read().decode())
                time.sleep(0.05)
            info = json.loads((data / "service.json").read_text())
            origin = f"http://127.0.0.1:{info['port']}"
            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))

            def http(path, body=None, method=None, custom_origin=None):
                headers = {"Origin": custom_origin or origin}
                if body is not None:
                    headers["Content-Type"] = "application/json"
                request = urllib.request.Request(origin + path, data=json.dumps(body).encode() if body is not None else None,
                                                 headers=headers, method=method)
                with opener.open(request, timeout=10) as response:
                    return json.loads(response.read())

            assert rpc("status")["worker"]["pid"] == 0
            try:
                http("/api/v1/settings")
                raise AssertionError("Unauthenticated history accepted")
            except urllib.error.HTTPError as error:
                assert error.code == 401
            url = rpc("pair")["url"]
            code = urllib.parse.parse_qs(urllib.parse.urlparse(url).fragment)["pair"][0]
            assert http("/api/v1/pair", {"code": code})["paired"]
            assert http("/api/v1/settings")["recording_enabled"] is False
            assert rpc("status")["worker"]["pid"] == 0
            try:
                http("/api/v1/settings", {"recording_enabled": True}, "PATCH", "https://wrong.invalid")
                raise AssertionError("Cross-origin write accepted")
            except urllib.error.HTTPError as error:
                assert error.code == 403
            http("/api/v1/settings", {"recording_enabled": True}, "PATCH")
            large_state = "external evidence " + "x" * (20 * 1024)
            large_decision = rpc("predict", {"state": large_state, "advisor": {"models": []}}, "large-evidence")
            assert large_decision["meta"]["recording_status"] == "stored", large_decision
            large_id = large_decision["meta"]["decision_id"]
            large_detail = http(f"/api/v1/decisions/{large_id}")
            request_snapshot = next(item for item in large_detail["snapshots"] if item["kind"] == "request")
            assert request_snapshot["artifact_id"] and request_snapshot["payload"]["state"] == large_state
            evidence_file = data / "evidence" / f"{request_snapshot['artifact_id']}.json"
            assert evidence_file.is_file()
            with sqlite3.connect(data / "outbox.sqlite3") as db:
                assert db.execute("SELECT count(*) FROM snapshots WHERE decision_id=?", (large_id,)).fetchone()[0] == 0
            http(f"/api/v1/decisions/{large_id}", method="DELETE")
            assert not evidence_file.exists()
            with ThreadPoolExecutor(max_workers=3) as clients:
                replies = list(clients.map(lambda n: rpc("predict", {"state": f"parallel {n}", "questions": {"risk": {}}}), range(3)))
            assert len(replies) == 3
            worker_pid = rpc("status")["worker"]["pid"]
            assert worker_pid > 0
            try:
                rpc("predict", {"state": "fixture:crash", "questions": {"risk": {}}})
                raise AssertionError("Worker crash was hidden")
            except RuntimeError:
                pass
            assert rpc("status")["worker"]["pid"] == 0
            decision = rpc("predict", {"state": "Check a rollback migration", "advisor": {"models": [], "task_family": "migration", "task_lineage": "smoke-rollback-review"}}, "decision-smoke")
            assert decision["meta"]["recording_status"] == "stored", decision
            tester = {"protocol_version": 1, "event_id": "tester-pass", "decision_id": "decision-smoke", "attempt_ref": "attempt-1",
                      "kind": "test", "source": {"host": "codex", "role": "tester", "actor_type": "agent"},
                      "payload": {"result": "pass", "summary": "Scoped tests pass; this does not establish rollback safety"}}
            tester_request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "laya_feedback", "arguments": tester}}
            tester_reply = subprocess.run([str(BINARY), "mcp"], input=json.dumps(tester_request)+"\n", env=env, text=True, capture_output=True, timeout=10, check=True)
            assert json.loads(tester_reply.stdout)["result"]["structuredContent"]["status"] == "stored"
            event = {"protocol_version": 1, "event_id": "first-score", "decision_id": "decision-smoke", "attempt_ref": "attempt-1",
                     "kind": "review", "source": {"host": "codex", "role": "reviewer", "actor_type": "agent"},
                     "payload": {"outcome": "changes_requested", "proposed_labels": {"risk": "high"}, "scores": [{"rubric_version": "laya-feedback-v1", "dimension": "judgment_quality", "value": 0,
                     "reason": "Missing rollback plan", "evidence_refs": [], "phase": "initial", "source_sequence": 1, "observed_at": "2026-09-25T00:00:00Z"}]}}
            tool_request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "laya_feedback", "arguments": event}}
            reply = subprocess.run([str(BINARY), "mcp"], input=json.dumps(tool_request)+"\n", env=env, text=True, capture_output=True, timeout=10, check=True)
            result = json.loads(reply.stdout)["result"]
            assert result.get("structuredContent", {}).get("status") == "stored", result
            assert rpc("feedback", event)["status"] == "stored"
            disagreement = http("/api/v1/decisions/decision-smoke")
            assert disagreement["reviews"] == [], disagreement
            assert disagreement["risk"] == "low", disagreement
            assert {item["event_id"] for item in disagreement["feedback"]} == {"tester-pass", "first-score"}
            pending = http("/api/v1/decisions?filter=pending")["items"]
            assert len(pending) == 1 and pending[0]["id"] == "decision-smoke", pending
            assert "review_disagreement" in pending[0]["review_reasons"], pending
            assert "reported_high_risk_correction" in pending[0]["review_reasons"], pending
            # Kill an offline MCP bridge only after its independent outbox commit.
            rpc("stop")
            service.wait(timeout=5)
            initial = copy.deepcopy(event)
            initial["event_id"] = "offline-initial"
            initial["attempt_ref"] = "offline-attempt"
            revision = copy.deepcopy(initial)
            revision["event_id"] = "offline-revision"
            revision["payload"]["scores"][0].update({"phase": "revision", "source_sequence": 2, "supersedes_event_id": "offline-initial", "value": 1})
            with (data / "service.lock").open("r+") as held_lock:
                fcntl.flock(held_lock, fcntl.LOCK_EX)
                for offline_event in (revision, initial):
                    bridge = subprocess.Popen([str(BINARY), "mcp"], env=env, text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
                    message = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "laya_feedback", "arguments": offline_event}}
                    bridge.stdin.write(json.dumps(message) + "\n")
                    bridge.stdin.flush()
                    committed = False
                    for _ in range(100):
                        with sqlite3.connect(data / "outbox.sqlite3") as db:
                            committed = db.execute("SELECT count(*) FROM outbox WHERE event_id=?", (offline_event["event_id"],)).fetchone()[0] == 1
                        if committed:
                            break
                        time.sleep(0.02)
                    assert committed, "Bridge never durably queued the event"
                    bridge.kill()
                    bridge.wait(timeout=5)
                fcntl.flock(held_lock, fcntl.LOCK_UN)
            service = subprocess.Popen([str(BINARY), "service"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            for _ in range(100):
                try:
                    if rpc("status")["service"]["pid"] == service.pid:
                        break
                except (OSError, RuntimeError):
                    pass
                time.sleep(0.05)
            info = rpc("status")["service"]
            origin = f"http://127.0.0.1:{info['port']}"
            code = urllib.parse.parse_qs(urllib.parse.urlparse(rpc("pair")["url"]).fragment)["pair"][0]
            http("/api/v1/pair", {"code": code})
            for _ in range(300):
                detail = http("/api/v1/decisions/decision-smoke")
                if len(detail["feedback"]) == 4:
                    break
                time.sleep(0.05)
            assert len(detail["feedback"]) == 4, detail
            stored_initial = next(item for item in detail["feedback"] if item["event_id"] == "offline-initial")
            assert stored_initial["payload"]["scores"][0]["value"] == 0
            detail = http("/api/v1/decisions/decision-smoke")
            assert len(detail["feedback"]) == 4
            assert next(item for item in detail["feedback"] if item["event_id"] == "first-score")["payload"]["scores"][0]["value"] == 0
            assert next(item for item in detail["feedback"] if item["event_id"] == "tester-pass")["payload"]["result"] == "pass"
            review = http("/api/v1/decisions/decision-smoke/reviews", {"expected_revision": 0, "status": "corrected", "labels": {"complexity": "high", "risk": "high", "certainty": "clear"}, "task_family": "migration", "task_lineage": "smoke-rollback-review", "language": "en", "applicability": "task-fact", "reason": "Rollback has data-loss consequences"})
            version = http("/api/v1/memory-versions", {"case_ids": [review["case_id"]]})
            version_id = version.get("id") or version.get("version", {}).get("id")
            assert version_id, version
            try:
                http(f"/api/v1/memory-versions/{version_id}/activate", {})
                raise AssertionError("Unreviewed evaluation activated")
            except urllib.error.HTTPError as error:
                assert error.code == 400
            assert http("/api/v1/decisions?filter=pending")["items"] == []
            job = http("/api/v1/jobs", {"kind": "evaluation", "version_id": version_id})
            for _ in range(100):
                job = http(f"/api/v1/jobs/{job['id']}")
                if job["status"] in ("completed", "failed", "cancelled"):
                    break
                time.sleep(0.05)
            assert job["status"] == "completed", job
            assert job["result"]["candidate_memory_exposure"] > 0, job
            assert job["result"]["passed"] is True, job
            http(f"/api/v1/memory-versions/{version_id}/activate", {})
            http("/api/v1/settings", {"memory_enabled": True}, "PATCH")
            remembered = rpc("predict", {"state": "Check a rollback migration for another service", "advisor": {"models": [], "task_family": "migration", "task_lineage": "smoke-rollback-reuse"}}, "with-memory")
            assert remembered["meta"]["memory_version"] == version_id, remembered
            assert review["case_id"] in remembered["meta"]["case_ids"], remembered
            assert review["case_id"] in remembered["meta"]["worker_case_ids"], remembered
            same_lineage = rpc("predict", {"state": "Check a rollback migration", "advisor": {"models": [], "task_family": "migration", "task_lineage": "smoke-rollback-review"}}, "same-lineage")
            assert same_lineage["meta"]["case_ids"] == [], same_lineage
            assert same_lineage["meta"]["worker_case_ids"] == [], same_lineage
            http("/api/v1/decisions/same-lineage", method="DELETE")
            exported = http("/api/v1/jobs", {"kind": "export"})
            for _ in range(100):
                exported = http(f"/api/v1/jobs/{exported['id']}")
                if exported["status"] in ("completed", "failed"):
                    break
                time.sleep(0.05)
            assert exported["status"] == "completed", exported
            artifact_url = origin + "/api/v1/exports/" + exported["result"]["artifact_id"]
            with opener.open(artifact_url) as response:
                records = [json.loads(line) for line in response]
            assert records[0]["type"] == "manifest"
            assert len(records) == 3
            backup = http("/api/v1/backups", {})
            http("/api/v1/decisions/decision-smoke", method="DELETE")
            try:
                opener.open(artifact_url)
                raise AssertionError("Deleted decision export retained")
            except urllib.error.HTTPError as error:
                assert error.code == 404
            http(f"/api/v1/backups/{backup['id']}/restore", {})
            assert (data / "backups" / f"{backup['id']}.sqlite3").is_file()
            try:
                http("/api/v1/backups", {"id": "../outside"})
                raise AssertionError("Unsafe backup path accepted")
            except urllib.error.HTTPError as error:
                assert error.code == 400
            assert http(f"/api/v1/backups/{backup['id']}", method="DELETE")["deleted"]
            assert not (data / "backups" / f"{backup['id']}.sqlite3").exists()
            assert all(item["id"] != backup["id"] for item in http("/api/v1/backups")["items"])
            try:
                http(f"/api/v1/backups/{backup['id']}/restore", {})
                raise AssertionError("Deleted backup could still be restored")
            except urllib.error.HTTPError as error:
                assert error.code == 404
            try:
                http("/api/v1/decisions/decision-smoke")
                raise AssertionError("Restore resurrected deleted decision")
            except urllib.error.HTTPError as error:
                assert error.code == 400 or error.code == 404
            try:
                rpc("feedback", event)
                raise AssertionError("Deleted decision resurrected")
            except RuntimeError:
                pass
            time.sleep(1.3)
            assert rpc("status")["worker"]["pid"] == 0
            bridge = subprocess.Popen([str(BINARY), "mcp"], env=env, text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
            bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "id": "cancel-me", "method": "tools/call", "params": {"name": "laya_tell_me", "arguments": {"state": "fixture:slow", "questions": {"risk": {}}}}}) + "\n")
            bridge.stdin.flush()
            for _ in range(100):
                if rpc("status")["worker"]["busy"]:
                    break
                time.sleep(0.02)
            assert rpc("status")["worker"]["busy"]
            bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "cancel-me"}}) + "\n")
            bridge.stdin.close()
            bridge.wait(timeout=5)
            for _ in range(100):
                if rpc("status")["worker"]["pid"] == 0:
                    break
                time.sleep(0.02)
            assert rpc("status")["worker"]["pid"] == 0
            print("Workbench smoke passed: pairing, origin, lazy worker, MCP feedback, idempotence, review, evaluation, activation, retrieval, streamed export, privacy deletion, restore, idle release")
        finally:
            try:
                rpc("stop")
            except Exception:
                service.terminate()
            try:
                service.wait(timeout=5)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait()


if __name__ == "__main__":
    main()
