"""Exercise live activity through an isolated Rust service and fixture worker."""
from concurrent.futures import ThreadPoolExecutor
from http.cookiejar import CookieJar
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo


ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("LAYA_TEST_BINARY", ROOT / "target/debug/laya"))
FIXTURE = ROOT / "tests/fixtures/workbench_worker.py"
QUESTIONS = {"risk": {"type": "choice", "instructions": "Risk?", "criteria": ["low", "high"]}}


class Service:
    def __init__(self, root):
        self.root = root
        self.process = subprocess.Popen([str(BINARY), "service"], env={**os.environ,
            "LAYA_WORKBENCH_DIR": str(root), "LAYA_PORT": "0", "LAYA_PYTHON": str(FIXTURE)},
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        for _ in range(200):
            if (root / "service.json").exists():
                info = json.loads((root / "service.json").read_text())
                if info["pid"] == self.process.pid:
                    try:
                        with socket.socket(socket.AF_UNIX) as probe:
                            probe.connect(str(root / "service.sock"))
                        break
                    except OSError:
                        pass
            if self.process.poll() is not None:
                error = self.process.stderr.read().decode()
                self.process.stdout.close()
                self.process.stderr.close()
                raise AssertionError(error)
            time.sleep(0.025)
        else:
            raise TimeoutError("service did not start")
        self.origin = f"http://127.0.0.1:{info['port']}"
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))

    def rpc(self, method, params=None, request_id=None):
        request = {"protocol_version": 1, "request_id": request_id or f"activity-{time.time_ns()}",
                   "method": method, "params": params or {}}
        with socket.socket(socket.AF_UNIX) as channel:
            channel.settimeout(10)
            channel.connect(str(self.root / "service.sock"))
            channel.sendall(json.dumps(request).encode() + b"\n")
            reply = json.loads(channel.makefile("rb").readline())
        if "error" in reply:
            raise RuntimeError(reply["error"])
        return reply["result"]

    def http(self, path, body=None, method=None, origin=None):
        headers = {"Origin": origin or self.origin}
        if body is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(self.origin + "/api/v1/" + path,
            data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
        with self.opener.open(request, timeout=10) as response:
            return json.loads(response.read())

    def pair(self):
        url = urllib.parse.urlparse(self.rpc("pair")["url"])
        code = urllib.parse.parse_qs(url.fragment)["pair"][0]
        self.http("pair", {"code": code})

    def close(self):
        try:
            self.rpc("stop")
        except (OSError, RuntimeError):
            self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.process.stdout.close()
        self.process.stderr.close()


@unittest.skipUnless(BINARY.is_file(), "build the Rust service first")
class ActivitySmokeTests(unittest.TestCase):
    def test_pending_sse_completion_failure_consent_and_deletion(self):
        with tempfile.TemporaryDirectory(prefix="laya-activity-", dir="/private/tmp") as directory:
            service = Service(Path(directory))
            try:
                now = int(time.time())
                start = now - 3600
                end = now + 3600
                path = f"activity?created_after={start}&created_before={end}&limit=50"
                with self.assertRaises(urllib.error.HTTPError) as denied:
                    service.http(path)
                self.assertEqual(denied.exception.code, 401)
                denied.exception.close()
                service.pair()
                empty = service.http(path)
                self.assertEqual(empty["scope"], "recorded_decisions")
                self.assertEqual(empty["items_scope"], "selected_recorded_decisions")
                self.assertFalse(empty["recording_enabled"])
                self.assertEqual(empty["totals"], {"decisions": 0, "completed": 0, "failed": 0, "pending": 0})
                self.assertEqual(sum(bucket["count"] for bucket in empty["buckets"]), 0)
                self.assertEqual(empty["bucket_seconds"], 900)
                self.assertEqual(empty["worker"]["pid"], 0)
                self.assertIsInstance(empty["observed_at"], int)
                self.assertIsInstance(empty["privacy_revision"], str)
                self.assertTrue(empty["privacy_revision"])
                self.assertIsInstance(empty["service_instance"], str)
                self.assertTrue(empty["service_instance"])
                for bad in ("activity", "activity?created_after=x&created_before=2",
                            "activity?created_after=2&created_before=2", "activity?created_after=-1&created_before=2",
                            "activity?created_after=0&created_before=1&limit=0",
                            "activity?created_after=0&created_before=1&limit=101",
                            "activity?created_after=0&created_before=1&offset=-1"):
                    with self.subTest(bad=bad), self.assertRaises(urllib.error.HTTPError) as error:
                        service.http(bad)
                    self.assertEqual(error.exception.code, 400)
                    error.exception.close()
                with self.assertRaises(urllib.error.HTTPError) as wrong_origin:
                    service.http("settings", {"recording_enabled": True}, "PATCH", "https://wrong.invalid")
                self.assertEqual(wrong_origin.exception.code, 403)
                wrong_origin.exception.close()

                off = service.rpc("predict", {"state": "consent-off", "questions": QUESTIONS}, "activity-off")
                self.assertEqual(off["meta"]["recording_status"], "not_recorded")
                self.assertEqual(service.http(path)["items"], [])
                service.http("settings", {"recording_enabled": True}, "PATCH")
                baseline = service.http(path)
                self.assertEqual(baseline["privacy_revision"], empty["privacy_revision"])
                self.assertEqual(baseline["service_instance"], empty["service_instance"])

                events = queue.Queue()
                stream = self.opener_stream(service)
                def read_events():
                    event = None
                    try:
                        for raw in stream:
                            line = raw.decode().strip()
                            if line.startswith("event: "):
                                event = line[7:]
                            elif line.startswith("data: ") and event == "change":
                                events.put((time.monotonic(), json.loads(line[6:])))
                    except (OSError, ValueError):
                        pass
                reader = threading.Thread(target=read_events, daemon=True)
                reader.start()
                with ThreadPoolExecutor(max_workers=1) as executor:
                    launched = time.monotonic()
                    result = executor.submit(service.rpc, "predict", {"state": "fixture:activity-slow", "questions": QUESTIONS}, "activity-slow")
                    created_event = self.await_event(events, "decision.created", "activity-slow", 1.5)
                    self.assertLess(created_event - launched, 1.5, "SSE creation should arrive during two-second inference")
                    pending = service.http(path)
                    self.assertEqual(pending["privacy_revision"], baseline["privacy_revision"])
                    self.assertEqual(pending["service_instance"], baseline["service_instance"])
                    item = next(item for item in pending["items"] if item["id"] == "activity-slow")
                    self.assertEqual(item["status"], "pending")
                    self.assertIsNone(item["finished_at"])
                    self.assertEqual(pending["totals"], {"decisions": 1, "completed": 0, "failed": 0, "pending": 1})
                    self.assertEqual(sum(bucket["count"] for bucket in pending["buckets"]), 1)
                    self.assertGreater(pending["worker"]["pid"], 0)
                    self.assertFalse(result.done(), "pending record must precede inference result")
                    self.assertEqual(result.result(timeout=5)["meta"]["recording_status"], "stored")
                self.await_event(events, "decision.finished", "activity-slow", 2)
                finished = service.http(path)
                self.assertEqual(finished["privacy_revision"], baseline["privacy_revision"])
                self.assertEqual(finished["service_instance"], baseline["service_instance"])
                item = next(item for item in finished["items"] if item["id"] == "activity-slow")
                self.assertEqual(item["status"], "completed")
                self.assertIsNotNone(item["finished_at"])
                self.assertEqual(item["risk"], "low")
                self.assertEqual(finished["totals"], {"decisions": 1, "completed": 1, "failed": 0, "pending": 0})

                with self.assertRaises(RuntimeError):
                    service.rpc("predict", {"state": "fixture:crash", "questions": QUESTIONS}, "activity-failed")
                failed = service.http(path)
                self.assertEqual(failed["privacy_revision"], baseline["privacy_revision"])
                self.assertEqual(failed["service_instance"], baseline["service_instance"])
                self.assertEqual(next(item for item in failed["items"] if item["id"] == "activity-failed")["status"], "failed")
                self.assertEqual(failed["totals"], {"decisions": 2, "completed": 1, "failed": 1, "pending": 0})
                self.assertEqual(sum(bucket["failed"] for bucket in failed["buckets"]), 1)
                service.http("decisions/activity-failed", method="DELETE")
                deleted = service.http(path)
                self.assertNotEqual(deleted["privacy_revision"], baseline["privacy_revision"])
                self.assertEqual(deleted["service_instance"], baseline["service_instance"])
                self.assertEqual(deleted["totals"]["decisions"], 1)
                self.assertNotIn("activity-failed", {item["id"] for item in deleted["items"]})
                service.http("settings", {"recording_enabled": False}, "PATCH")
                service.rpc("predict", {"state": "consent-off-again", "questions": QUESTIONS}, "activity-off-again")
                disabled = service.http(path)
                self.assertEqual(disabled["privacy_revision"], deleted["privacy_revision"])
                self.assertEqual(disabled["service_instance"], baseline["service_instance"])
                self.assertFalse(disabled["recording_enabled"])
                self.assertEqual(disabled["totals"]["decisions"], 1)
                self.assertEqual(len(disabled["items"]), 1)
                stream.close()
            finally:
                service.close()

    def test_day_ranges_and_latest_items_share_interval(self):
        with tempfile.TemporaryDirectory(prefix="laya-activity-days-", dir="/private/tmp") as directory:
            service = Service(Path(directory))
            try:
                service.pair()
                service.http("settings", {"recording_enabled": True}, "PATCH")
                marker = "private-advisor-payload-7f4c"
                result = service.rpc("predict", {"state": "public summary", "questions": QUESTIONS,
                    "advisor": {"models": [], "private_note": marker}}, "activity-latest")
                self.assertEqual(result["meta"]["recording_status"], "stored")
                stamp = service.http("decisions/activity-latest")["created_at"]
                stamp_ms = service.http("decisions/activity-latest")["created_at_ms"]
                self.assertEqual(stamp_ms // 1000, stamp)
                for start, end, expected in ((stamp, stamp + 900, 1), (stamp - 900, stamp, 0),
                                             (stamp + 1, stamp + 901, 0)):
                    activity = service.http(f"activity?created_after={start}&created_before={end}&limit=1")
                    self.assertEqual(activity["totals"]["decisions"], expected)
                    self.assertEqual(activity["buckets"][0]["count"], expected)
                    self.assertEqual([item["id"] for item in activity["items"]], ["activity-latest"] if expected else [])
                    if expected:
                        self.assertEqual(activity["items"][0]["created_at_ms"], stamp_ms)
                    self.assertNotIn(marker, json.dumps(activity))
                zone = ZoneInfo("America/New_York")
                for day, hours in ((datetime(2026, 3, 8, tzinfo=zone), 23),
                                   (datetime(2026, 11, 1, tzinfo=zone), 25)):
                    start = int(day.timestamp())
                    end = int((day + timedelta(days=1)).timestamp())
                    self.assertEqual(end - start, hours * 3600)
                    activity = service.http(f"activity?created_after={start}&created_before={end}")
                    self.assertEqual(len(activity["buckets"]), hours * 4)
                    self.assertEqual(activity["buckets"][0]["start"], start)
                    self.assertEqual(activity["buckets"][-1]["end"], end)
                    self.assertEqual(activity["totals"]["decisions"], 0)
                    self.assertEqual(activity["items"], [])
                long_range = service.http("activity?created_after=0&created_before=77760000")
                self.assertEqual(long_range["bucket_seconds"], 3 * 86400)
                self.assertEqual(len(long_range["buckets"]), 300)
            finally:
                service.close()

    @staticmethod
    def opener_stream(service):
        request = urllib.request.Request(service.origin + "/api/v1/events", headers={"Origin": service.origin})
        return service.opener.open(request, timeout=3)

    @staticmethod
    def await_event(events, kind, decision_id, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                received, event = events.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                break
            if event.get("kind") == kind and event.get("payload", {}).get("id") == decision_id:
                return received
        raise AssertionError(f"SSE did not deliver {kind} for {decision_id} within {timeout}s")


if __name__ == "__main__":
    unittest.main()
