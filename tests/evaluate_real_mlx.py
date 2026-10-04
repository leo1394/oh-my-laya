"""Opt-in MLX evaluation; optional lifecycle test touches only disposable data."""
from http.cookiejar import CookieJar
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("LAYA_TEST_BINARY", ROOT / "target/debug/laya")).resolve()


def main():
    for variable in ("LAYA_PYTHON", "LAYA_MODEL_DIR"):
        if not os.environ.get(variable):
            raise SystemExit(f"Set {variable} explicitly; no global configuration is changed")
    with tempfile.TemporaryDirectory(prefix="laya-real-eval-", dir="/private/tmp") as directory:
        env = {**os.environ, "LAYA_WORKBENCH_DIR": directory, "LAYA_PORT": "0", "LAYA_ADVISOR_CONFIG": directory + "/advisor.json", "PYTHONPATH": str(ROOT / "src")}
        process = subprocess.Popen([str(BINARY), "service"], env=env)

        def rpc(method, params=None):
            with socket.socket(socket.AF_UNIX) as channel:
                channel.settimeout(125)
                channel.connect(directory + "/service.sock")
                channel.sendall(json.dumps({"protocol_version": 1, "request_id": f"eval-{time.time_ns()}", "method": method, "params": params or {}}).encode() + b"\n")
                reply = json.loads(channel.makefile("rb").readline())
                if "error" in reply:
                    raise RuntimeError(reply["error"])
                return reply["result"]

        try:
            for _ in range(100):
                if (Path(directory) / "service.json").exists():
                    break
                if process.poll() is not None:
                    raise RuntimeError("Disposable evaluation service exited during startup")
                time.sleep(0.05)
            else:
                raise RuntimeError("Disposable evaluation service startup timed out")
            url = rpc("pair")["url"]
            parsed = urllib.parse.urlparse(url)
            origin = f"{parsed.scheme}://{parsed.netloc}"
            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))

            def http(path, body=None, method=None):
                request = urllib.request.Request(origin + "/api/v1" + path,
                    data=json.dumps(body).encode() if body is not None else None,
                    headers={"Origin": origin, "Content-Type": "application/json"}, method=method)
                with opener.open(request, timeout=15) as response:
                    return json.loads(response.read())

            http("/pair", {"code": urllib.parse.parse_qs(parsed.fragment)["pair"][0]})
            http("/settings", {"recording_enabled": True}, "PATCH")
            prediction = rpc("predict", {"state": "Migrate the production database and permanently remove the old encrypted backup without testing restore.", "advisor": {"models": [{"id": "evaluation-fixture", "reasoning_efforts": ["low", "medium", "high"]}], "current_model": "evaluation-fixture", "task_family": "migration", "task_lineage": "integration-backup-retirement", "language": "en"}})
            # Simulate the authenticated human-review UI in a disposable test database.
            # This is not evidence of an actual user's review or learning approval.
            review = http(f"/decisions/{prediction['meta']['decision_id']}/reviews", {
                "expected_revision": 0, "status": "corrected", "labels": {"complexity": "high", "risk": "high", "certainty": "clear"},
                "reason": "Synthetic integration annotation: destructive production backup changes", "task_family": "migration", "task_lineage": "integration-backup-retirement", "language": "en", "applicability": "task-fact"})
            version = http("/memory-versions", {"case_ids": [review["case_id"]]})
            started = time.monotonic()
            job = http("/jobs", {"kind": "evaluation", "version_id": version["id"]})
            for _ in range(600):
                job = http(f"/jobs/{job['id']}")
                if job["status"] in ("completed", "failed", "cancelled"):
                    break
                time.sleep(1)
            report = job.get("result") or {}
            if os.environ.get("LAYA_EVAL_REPORT"):
                # Persist synthetic evaluation evidence only when explicitly requested.
                with Path(os.environ["LAYA_EVAL_REPORT"]).open("x") as output:
                    json.dump({"provenance": "synthetic fixture simulating authenticated human review; not an actual user review", "job": job}, output, indent=2)
            print(json.dumps({"job_status": job["status"], "error": job.get("error"),
                "elapsed_seconds": round(time.monotonic() - started, 2),
                "sample_count": report.get("sample_count"), "passed": report.get("passed"),
                "dimensions": report.get("dimensions"), "identity_verified": report.get("identity_verified"),
                "new_high_to_low": report.get("new_high_to_low"), "invalid_outputs": report.get("invalid_outputs"),
                "uncertain_asks": report.get("uncertain_asks"), "uncertain_total": report.get("uncertain_total"),
                "candidate_memory_exposure": report.get("candidate_memory_exposure"),
                "activation": "none during evaluation; optional disposable lifecycle follows"}))
            if job["status"] != "completed":
                raise AssertionError(job)
            if not report.get("identity_verified") or report.get("candidate_memory_exposure", 0) < 1:
                raise AssertionError("Evaluation did not verify model identity and exercise candidate memory")
            if report.get("passed") is not True:
                raise AssertionError("Candidate failed the unchanged evaluation quality gates")
            for row in report["rows"]:
                for run in row["runs"]:
                    assert run["result"].get("meta", {}).get("case_ids") == run["case_ids"], row
            migration = next(row for row in report["rows"] if row["id"] == "migration-en")
            assert review["case_id"] in migration["runs"][2]["case_ids"], migration
            assert migration["task_lineage"] != "integration-backup-retirement", migration
            if rpc("status")["settings"]["active_memory_version"] is not None:
                raise AssertionError("Evaluation unexpectedly activated a memory version")
            if os.environ.get("LAYA_EVAL_LIFECYCLE") == "1":
                # Explicit opt-in simulates approval only in this owned temporary directory.
                http(f"/memory-versions/{version['id']}/activate", {})
                http("/settings", {"memory_enabled": True}, "PATCH")

                def native_prediction():
                    request = {"jsonrpc": "2.0", "id": "lifecycle", "method": "tools/call", "params": {
                        "name": "laya_tell_me", "arguments": {
                            "state": "Review a production database migration recovery plan for an independent billing service.",
                            "advisor": {"models": [{"id": "evaluation-fixture", "reasoning_efforts": ["low", "medium", "high"]}],
                                        "current_model": "evaluation-fixture", "task_family": "migration",
                                        "task_lineage": "integration-billing-recovery"}}}}
                    completed = subprocess.run([str(BINARY), "mcp"], input=json.dumps(request) + "\n",
                                               env=env, text=True, capture_output=True, timeout=125, check=True)
                    result = json.loads(completed.stdout)["result"]
                    assert not result.get("isError"), result
                    return result["structuredContent"]

                try:
                    remembered = native_prediction()
                    assert remembered["meta"]["memory_version"] == version["id"], remembered
                    assert remembered["meta"]["case_ids"] == [review["case_id"]], remembered
                    assert remembered["meta"]["worker_case_ids"] == [review["case_id"]], remembered
                finally:
                    http("/settings", {"memory_enabled": False}, "PATCH")
                disabled = native_prediction()
                assert disabled["meta"]["case_ids"] == [], disabled
                assert disabled["meta"]["worker_case_ids"] == [], disabled
                assert rpc("status")["settings"]["memory_enabled"] is False
                lifecycle = {"lifecycle": "passed", "scope": "disposable simulated user review and activation; actual MLX over MCP stdio",
                                  "version_id": version["id"], "case_id": review["case_id"],
                                  "enabled": remembered["meta"], "disabled": disabled["meta"]}
                if os.environ.get("LAYA_EVAL_REPORT"):
                    with Path(os.environ["LAYA_EVAL_REPORT"] + ".lifecycle.json").open("x") as output:
                        json.dump(lifecycle, output, indent=2)
                print(json.dumps(lifecycle))
        finally:
            try:
                rpc("stop")
            except Exception:
                process.terminate()
            process.wait(timeout=10)


if __name__ == "__main__":
    main()
