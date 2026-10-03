"""Real service fault injection with a deterministic, non-MLX worker."""
from http.cookiejar import CookieJar
import json
import os
from pathlib import Path
import socket
import signal
import sqlite3
import subprocess
import tempfile
import time
import unittest
import urllib.parse
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/debug/laya"


class Service:
    def __init__(self, root):
        self.root = root
        self.process = subprocess.Popen([str(BINARY), "service"], env={**os.environ,
            "LAYA_WORKBENCH_DIR": str(root), "LAYA_PYTHON": str(ROOT / "tests/fixtures/workbench_worker.py")},
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        for _ in range(250):
            if (root / "service.json").exists():
                try:
                    if json.loads((root / "service.json").read_text())["pid"] == self.process.pid:
                        with socket.socket(socket.AF_UNIX) as probe:
                            probe.settimeout(0.1)
                            probe.connect(str(root / "service.sock"))
                        break
                except (ValueError, OSError):
                    pass
            if self.process.poll() is not None:
                raise AssertionError(self.process.stderr.read().decode())
            time.sleep(0.02)
        else:
            self.process.terminate()
            self.process.wait(timeout=5)
            self.process.stdout.close()
            self.process.stderr.close()
            raise TimeoutError("Service did not become ready within five seconds")

    def rpc(self, method, params=None, request_id="fault-check"):
        with socket.socket(socket.AF_UNIX) as channel:
            channel.settimeout(10)
            channel.connect(str(self.root / "service.sock"))
            channel.sendall(json.dumps({"protocol_version": 1, "request_id": request_id,
                                       "method": method, "params": params or {}}).encode() + b"\n")
            reply = json.loads(channel.makefile("rb").readline())
        if "error" in reply:
            raise RuntimeError(reply["error"])
        return reply["result"]

    def consent(self):
        url = urllib.parse.urlparse(self.rpc("pair")["url"])
        origin = f"{url.scheme}://{url.netloc}"
        opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))
        for path, body, method in (("pair", {"code": urllib.parse.parse_qs(url.fragment)["pair"][0]}, "POST"),
                                   ("settings", {"recording_enabled": True}, "PATCH")):
            request = urllib.request.Request(origin + "/api/v1/" + path, json.dumps(body).encode(),
                headers={"Origin": origin, "Content-Type": "application/json"}, method=method)
            with opener.open(request, timeout=5) as response:
                response.read()

    def close(self):
        try:
            self.rpc("stop")
        except Exception:
            self.process.terminate()
        self.process.wait(timeout=5)
        self.process.stdout.close()
        self.process.stderr.close()


@unittest.skipUnless(BINARY.exists(), "Build the Rust service first")
class ServiceFailureTests(unittest.TestCase):
    def test_both_databases_reject_writes_across_restart_then_recover(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp", prefix="laya-fault-") as directory:
            root = Path(directory)
            service = Service(root)
            try:
                service.consent()
                for filename, table in (("laya.sqlite3", "decisions"), ("outbox.sqlite3", "snapshots")):
                    with sqlite3.connect(root / filename) as db:
                        db.execute(f"CREATE TRIGGER rejected_write BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'simulated storage exhaustion'); END;")
                for phase in range(2):
                    for attempt in range(3):
                        result = service.rpc("predict", {"state": "documentation", "advisor": {"models": []}},
                                             request_id=f"unsaved-{phase}-{attempt}")
                        self.assertEqual(result["meta"]["recording_status"], "not_saved")
                    self.assertEqual(service.rpc("status")["counts"]["decisions"], 0)
                    self.assertEqual(service.rpc("status")["outbox"]["pending_snapshots"], 0)
                    if phase == 0:
                        service.close()
                        service = Service(root)
                for filename in ("laya.sqlite3", "outbox.sqlite3"):
                    with sqlite3.connect(root / filename) as db:
                        db.execute("DROP TRIGGER rejected_write")
                result = service.rpc("predict", {"state": "documentation", "advisor": {"models": []}},
                                     request_id="after-storage-recovery")
                self.assertEqual(result["meta"]["recording_status"], "stored")
                self.assertEqual(service.rpc("status")["counts"]["decisions"], 1)
            finally:
                service.close()

    def test_model_child_keeps_lock_after_service_sigkill(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp", prefix="laya-fault-") as directory:
            root = Path(directory)
            first = Service(root)
            second = None
            orphan = None
            channel = socket.socket(socket.AF_UNIX)
            try:
                channel.connect(str(root / "service.sock"))
                channel.sendall(json.dumps({"protocol_version": 1, "request_id": "orphan-test", "method": "predict",
                                           "params": {"state": "fixture:slow", "questions": {"risk": {}}}}).encode() + b"\n")
                for _ in range(100):
                    state = first.rpc("status")["worker"]
                    if state["busy"] and state["pid"]:
                        orphan = state["pid"]
                        break
                    time.sleep(0.02)
                self.assertIsNotNone(orphan)
                first.process.kill()
                first.process.wait(timeout=5)
                second = Service(root)
                with self.assertRaisesRegex(RuntimeError, "model resource is busy"):
                    second.rpc("predict", {"state": "another", "questions": {"risk": {}}})
                os.kill(orphan, signal.SIGTERM)
                orphan = None
                time.sleep(0.1)
                self.assertIn("laya_result", second.rpc("predict", {"state": "after orphan exit", "questions": {"risk": {}}}))
            finally:
                channel.close()
                if orphan is not None:
                    try:
                        os.kill(orphan, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                if second is not None:
                    second.close()
                if first.process.poll() is None:
                    first.close()
                else:
                    first.process.stdout.close()
                    first.process.stderr.close()

    def test_corrupt_database_is_not_replaced_and_inference_is_explicitly_unsaved(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp", prefix="laya-fault-") as directory:
            root = Path(directory)
            (root / "laya.sqlite3").write_bytes(b"test-only corrupt database")
            service = Service(root)
            try:
                self.assertTrue(service.rpc("status")["degraded"])
                result = service.rpc("predict", {"state": "documentation", "advisor": {"models": []}})
                self.assertEqual(result["meta"]["recording_status"], "not_saved")
                self.assertEqual((root / "laya.sqlite3").read_bytes(), b"test-only corrupt database")
            finally:
                service.close()

    def test_outbox_commit_failure_never_claims_snapshot_stored(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp", prefix="laya-fault-") as directory:
            root = Path(directory)
            service = Service(root)
            try:
                service.consent()
                service.rpc("status")
                with sqlite3.connect(root / "outbox.sqlite3") as db:
                    db.executescript("CREATE TRIGGER full_disk BEFORE INSERT ON snapshots BEGIN SELECT RAISE(ABORT,'disk full'); END;")
                result = service.rpc("predict", {"state": "documentation", "advisor": {"models": []}})
                self.assertEqual(result["meta"]["recording_status"], "not_saved")
                self.assertEqual(service.rpc("status")["counts"]["decisions"], 0)
            finally:
                service.close()

    def test_output_snapshot_replays_after_main_database_write_recovers(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp", prefix="laya-fault-") as directory:
            root = Path(directory)
            service = Service(root)
            try:
                service.consent()
                with sqlite3.connect(root / "laya.sqlite3") as db:
                    db.executescript("CREATE TRIGGER full_disk BEFORE UPDATE OF result_json ON decisions BEGIN SELECT RAISE(ABORT,'disk full'); END;")
                result = service.rpc("predict", {"state": "documentation", "advisor": {"models": []}})
                self.assertEqual(result["meta"]["recording_status"], "queued_local")
                self.assertEqual(service.rpc("status")["outbox"]["pending_snapshots"], 1)
                with sqlite3.connect(root / "laya.sqlite3") as db:
                    db.execute("DROP TRIGGER full_disk")
                for _ in range(120):
                    if service.rpc("status")["outbox"]["pending_snapshots"] == 0:
                        break
                    time.sleep(0.1)
                self.assertEqual(service.rpc("status")["outbox"]["pending_snapshots"], 0)
                with sqlite3.connect(root / "laya.sqlite3") as db:
                    self.assertIsNotNone(db.execute("SELECT result_json FROM decisions WHERE id='fault-check'").fetchone()[0])
            finally:
                service.close()


if __name__ == "__main__":
    unittest.main()
