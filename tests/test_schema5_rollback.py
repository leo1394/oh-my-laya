"""Opt-in released-binary schema migration and rollback acceptance."""
import hashlib
from contextlib import closing
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import unittest

from tests.test_orchestration_transport import OLD_WORKER, Service


CURRENT_SOURCE = os.environ.get("LAYA_TEST_BINARY")
LEGACY_SOURCE = os.environ.get("LAYA_LEGACY_TEST_BINARY")
CURRENT_BINARY = Path(CURRENT_SOURCE) if CURRENT_SOURCE else None
LEGACY_BINARY = Path(LEGACY_SOURCE) if LEGACY_SOURCE else None


class OwnedService(Service):
    """Service lifecycle owned by this test, including forced reap on timeout."""
    def close(self):
        if self.process.poll() is None:
            try:
                self.rpc("stop")
            except Exception:
                self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        finally:
            self.process.stdout.close()
            self.process.stderr.close()
        if self.process.poll() is None:
            raise AssertionError("temporary service was not reaped")


def file_hash(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def database_state(path, event_id):
    with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as connection:
        return {
            "schema": connection.execute("PRAGMA user_version").fetchone()[0],
            "integrity": connection.execute("PRAGMA integrity_check").fetchone()[0],
            "event_hash": connection.execute(
                "SELECT event_hash FROM feedback_events WHERE event_id=?", (event_id,)
            ).fetchone()[0],
        }


def logical_database(path, excluded_tables=()):
    with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as connection:
        schema = [row for row in connection.execute(
            "SELECT type,name,tbl_name,sql FROM sqlite_master "
            "WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name"
        ).fetchall() if row[1] not in excluded_tables and row[2] not in excluded_tables]
        tables = {}
        for kind, name, _table, _sql in schema:
            if kind != "table":
                continue
            columns = connection.execute(f'PRAGMA table_info("{name}")').fetchall()
            rows = connection.execute(f'SELECT * FROM "{name}"').fetchall()
            tables[name] = {"columns": columns, "rows": sorted(rows, key=repr)}
        return {"schema": schema, "tables": tables}


def changed_ranges(before, after):
    ranges = []
    start = None
    for offset in range(max(len(before), len(after))):
        changed = offset >= len(before) or offset >= len(after) or before[offset] != after[offset]
        if changed and start is None:
            start = offset
        if not changed and start is not None:
            ranges.append((start, offset - 1))
            start = None
    if start is not None:
        ranges.append((start, max(len(before), len(after)) - 1))
    return ranges


@unittest.skipUnless(
    CURRENT_SOURCE and LEGACY_SOURCE,
    "Set LAYA_TEST_BINARY and LAYA_LEGACY_TEST_BINARY for rollback acceptance",
)
class SchemaFiveRollbackTests(unittest.TestCase):
    def test_owned_service_kills_and_reaps_after_stop_timeout(self):
        class HangingProcess:
            def __init__(self):
                self.stdout = open(os.devnull, "rb")
                self.stderr = open(os.devnull, "rb")
                self.waits = []
                self.killed = False

            def poll(self):
                return -9 if self.killed else None

            def terminate(self):
                raise AssertionError("graceful RPC stop should be attempted first")

            def kill(self):
                self.killed = True

            def wait(self, timeout):
                self.waits.append(timeout)
                if not self.killed:
                    raise subprocess.TimeoutExpired("fixture", timeout)
                return -9

        service = object.__new__(OwnedService)
        service.process = HangingProcess()
        service.rpc = lambda method: {"stopped": method == "stop"}
        service.close()
        self.assertTrue(service.process.killed)
        self.assertEqual(service.process.waits, [5, 5])
        self.assertTrue(service.process.stdout.closed)
        self.assertTrue(service.process.stderr.closed)

    def test_rc3_current_rc3_rollback_preserves_recorded_event(self):
        self.assertTrue(CURRENT_BINARY.is_file(), CURRENT_BINARY)
        self.assertTrue(LEGACY_BINARY.is_file(), LEGACY_BINARY)
        self.assertTrue(os.access(CURRENT_BINARY, os.X_OK), CURRENT_BINARY)
        self.assertTrue(os.access(LEGACY_BINARY, os.X_OK), LEGACY_BINARY)

        with tempfile.TemporaryDirectory(prefix="laya-schema5-rollback-", dir="/private/tmp") as directory:
            root = Path(directory)
            event_id = "schema-five-rollback-event"
            decision_id = "schema-five-rollback-decision"
            event = {
                "protocol_version": 1,
                "event_id": event_id,
                "decision_id": decision_id,
                "attempt_ref": "attempt-1",
                "kind": "test",
                "source": {"host": "codex", "role": "tester", "actor_type": "agent"},
                "payload": {
                    "result": "pass",
                    "summary": "temporary schema rollback acceptance event",
                },
            }

            legacy = OwnedService(LEGACY_BINARY, root, OLD_WORKER)
            try:
                http = legacy.authenticated_http()
                self.assertTrue(
                    http("settings", {"recording_enabled": True}, "PATCH")["recording_enabled"]
                )
                result = legacy.rpc(
                    "predict",
                    {"state": "record schema rollback fixture", "advisor": {"models": []}},
                    decision_id,
                )
                self.assertEqual(result["meta"]["recording_status"], "stored")
                receipt = legacy.rpc("feedback", event, "schema-five-feedback")
                self.assertEqual(receipt["status"], "stored")
                expected_hash = receipt["payload_hash"]
            finally:
                legacy.close()

            database = root / "laya.sqlite3"
            legacy_state = database_state(database, event_id)
            legacy_logical = logical_database(database)
            self.assertEqual(legacy_state, {
                "schema": 4,
                "integrity": "ok",
                "event_hash": expected_hash,
            })

            current = OwnedService(CURRENT_BINARY, root, OLD_WORKER)
            try:
                self.assertEqual(current.rpc("status")["schema_version"], 5)
                http = current.authenticated_http()
                detail = http(f"decisions/{decision_id}")
                recorded = next(item for item in detail["feedback"] if item["event_id"] == event_id)
                self.assertEqual(recorded["payload_hash"], expected_hash)
            finally:
                current.close()

            archives = list((root / "backups").glob("pre-migration-*.sqlite3"))
            self.assertEqual(len(archives), 1, archives)
            archive = archives[0]
            archive_digest = file_hash(archive)
            self.assertEqual(database_state(archive, event_id), legacy_state)
            self.assertEqual(
                logical_database(archive, ("backup_evidence", "backup_evidence_damage")),
                legacy_logical,
            )
            migrated_state = database_state(database, event_id)
            self.assertEqual(migrated_state, {
                "schema": 5,
                "integrity": "ok",
                "event_hash": expected_hash,
            })

            before_rejection_bytes = database.read_bytes()
            before_rejection = hashlib.sha256(before_rejection_bytes).hexdigest()
            before_rejection_logical = logical_database(database)
            rejection_env = {
                **os.environ,
                "LAYA_WORKBENCH_DIR": str(root),
                "LAYA_PORT": "0",
                "LAYA_PYTHON": str(OLD_WORKER),
            }
            rejected = subprocess.run(
                [str(LEGACY_BINARY), "service"], env=rejection_env,
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(rejected.returncode, 0, rejected.stdout)
            self.assertIn("database schema 5 is newer than supported 4", rejected.stderr)
            after_rejection_bytes = database.read_bytes()
            after_rejection = hashlib.sha256(after_rejection_bytes).hexdigest()
            self.assertEqual(database_state(database, event_id), migrated_state)
            self.assertEqual(logical_database(database), before_rejection_logical)
            ranges = changed_ranges(before_rejection_bytes, after_rejection_bytes)
            page_size = int.from_bytes(before_rejection_bytes[16:18], "big") or 65536
            pages = sorted({page for start, end in ranges for page in range(start // page_size + 1, end // page_size + 2)})
            wal = root / "laya.sqlite3-wal"
            print(
                "rc3 schema5 rejection preserved all logical schema/rows; "
                f"physical_sha_changed={before_rejection != after_rejection} "
                f"changed_ranges={ranges} pages={pages} "
                f"wal_bytes={wal.stat().st_size if wal.exists() else 0}"
            )

            self.assertIsNotNone(rejected.returncode)
            with socket.socket(socket.AF_UNIX) as probe:
                self.assertNotEqual(probe.connect_ex(str(root / "service.sock")), 0)
            with closing(sqlite3.connect(database)) as connection:
                checkpoint = connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
            self.assertEqual(checkpoint, (0, 0, 0))
            self.assertFalse((root / "laya.sqlite3-wal").exists())
            self.assertFalse((root / "laya.sqlite3-shm").exists())
            (root / "service.sock").unlink(missing_ok=True)

            staged = root / ".rollback.sqlite3"
            shutil.copyfile(archive, staged)
            staged.chmod(0o600)
            with staged.open("rb") as handle:
                os.fsync(handle.fileno())
            os.replace(staged, database)
            for suffix in ("-wal", "-shm"):
                (root / f"laya.sqlite3{suffix}").unlink(missing_ok=True)
            directory_fd = os.open(root, os.O_RDONLY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
            self.assertEqual(file_hash(archive), archive_digest)
            self.assertEqual(database_state(database, event_id), legacy_state)
            self.assertEqual(database.stat().st_mode & 0o777, 0o600)

            rolled_back = OwnedService(LEGACY_BINARY, root, OLD_WORKER)
            try:
                status = rolled_back.rpc("status")
                self.assertEqual(status["schema_version"], 4)
                self.assertEqual(status["counts"]["decisions"], 1)
                http = rolled_back.authenticated_http()
                detail = http(f"decisions/{decision_id}")
                recorded = next(item for item in detail["feedback"] if item["event_id"] == event_id)
                self.assertEqual(recorded["payload_hash"], expected_hash)
            finally:
                rolled_back.close()
            self.assertEqual(file_hash(archive), archive_digest)
            self.assertEqual(database.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
