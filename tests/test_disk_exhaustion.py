"""Opt-in macOS ENOSPC acceptance on a bounded, disposable mounted image."""
from contextlib import closing
import errno
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest

from test_service_failures import BINARY, ROOT, Service


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, timeout=60).stdout


def image_entity(image, mount):
    info = plistlib.loads(run('/usr/bin/hdiutil', 'info', '-plist'))
    for item in info.get('images', []):
        if Path(item.get('image-path', '')).resolve() != image.resolve():
            continue
        for entity in item.get('system-entities', []):
            if entity.get('mount-point') == str(mount):
                for device in item.get('system-entities', []):
                    if re.fullmatch(r'/dev/disk[0-9]+', device.get('dev-entry', '')):
                        return device['dev-entry']
    raise AssertionError('Disposable image/mount identity cannot be verified')


def fill_volume(mount):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
    fd = os.open(mount / 'filler.bin', flags, 0o600)
    written = 0
    exhausted = False
    chunk = os.urandom(64 * 1024)
    try:
        for size in (len(chunk), 4096, 512):
            while written < 64 * 1024 * 1024:
                try:
                    written += os.write(fd, chunk[:size])
                    os.fsync(fd)
                except OSError as error:
                    if error.errno != errno.ENOSPC:
                        raise
                    exhausted = True
                    break
            else:
                raise AssertionError('Independent filler safety cap reached without ENOSPC')
        if not exhausted:
            raise AssertionError('The test volume did not report ENOSPC')
    finally:
        os.close(fd)
    return written


@unittest.skipUnless(sys.platform == 'darwin' and os.environ.get('LAYA_TEST_DISK_IMAGE') == '1',
                     'Opt in with LAYA_TEST_DISK_IMAGE=1 on macOS')
class DiskExhaustionTests(unittest.TestCase):
    def test_real_full_volume_preserves_initial_feedback_and_reports_write_failures(self):
        self.assertTrue(BINARY.is_file(), 'Build the debug service first')
        self.assertGreater(shutil.disk_usage('/private/tmp').free, 1024 * 1024 * 1024)
        directory = Path(tempfile.mkdtemp(prefix='laya-enospc-', dir='/private/tmp'))
        image = directory / 'disk.dmg'
        mount = directory / 'mount'
        mount.mkdir()
        attached = False
        verified_mount = False
        service = None
        try:
            run('/usr/bin/hdiutil', 'create', '-size', '32m', '-fs', 'HFS+',
                '-type', 'UDIF', '-volname', 'LayaFaultTest', str(image))
            # Mark ownership before attach: a timeout must preserve the backing image.
            attached = True
            run('/usr/bin/hdiutil', 'attach', str(image), '-mountpoint', str(mount), '-noautoopen', '-plist')
            device = image_entity(image, mount)
            self.assertTrue(device.startswith('/dev/disk'))
            self.assertNotEqual(mount.stat().st_dev, directory.stat().st_dev)
            stats = os.statvfs(mount)
            self.assertLessEqual(stats.f_blocks * stats.f_frsize, 64 * 1024 * 1024)
            verified_mount = True
            root = mount / 'workbench'
            root.mkdir(mode=0o700)
            service = Service(root)
            service.consent()
            first = service.rpc('predict', {'state': 'Initial reviewed task', 'advisor': {'models': []}}, 'initial')
            self.assertEqual(first['meta']['recording_status'], 'stored')
            event = {'protocol_version': 1, 'event_id': 'initial-score', 'decision_id': 'initial',
                     'attempt_ref': 'initial-attempt', 'kind': 'test',
                     'source': {'host': 'codex', 'role': 'tester', 'actor_type': 'agent'},
                     'payload': {'result': 'pass', 'scores': [{'rubric_version': 'laya-feedback-v1',
                         'dimension': 'judgment_quality', 'value': 1, 'reason': 'First observation',
                         'evidence_refs': [], 'phase': 'initial', 'source_sequence': 1,
                         'observed_at': '2026-10-04T00:00:00Z'}]}}
            receipt = service.rpc('feedback', event)
            self.assertEqual(receipt['status'], 'stored')
            with closing(sqlite3.connect(root / 'laya.sqlite3')) as db:
                initial = db.execute("SELECT payload_json,event_hash FROM feedback_events WHERE event_id='initial-score'").fetchone()
            filled = fill_volume(mount)
            probe_errno = None
            probe = None
            try:
                probe = os.open(mount / 'write-probe.bin', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                for _ in range(16):
                    os.write(probe, b'x' * 4096)
                    os.fsync(probe)
            except OSError as error:
                probe_errno = error.errno
            finally:
                if probe is not None:
                    os.close(probe)
            self.assertEqual(probe_errno, errno.ENOSPC)
            results = []
            for attempt in range(6):
                request_id = f'full-{attempt}'
                result = service.rpc('predict', {'state': 'x' * (240 * 1024), 'advisor': {'models': []}}, request_id)
                status = result['meta']['recording_status']
                self.assertIn(status, ('stored', 'queued_local', 'not_saved'))
                results.append((request_id, status, result['meta'].get('recording_error', '')))
            self.assertTrue(any(status != 'stored' for _, status, _ in results), results)
            failures = ' '.join(message for _, _, message in results).lower()
            self.assertTrue(any(text in failures for text in ('disk is full', 'database or disk is full', 'no space left', 'os error 28', 'unable to open database file')), failures)
            feedback_results = []
            pressure_events = []
            for attempt in range(3):
                pressure_event = json.loads(json.dumps(event))
                pressure_event['event_id'] = f'pressure-score-{attempt}'
                pressure_event['attempt_ref'] = f'pressure-attempt-{attempt}'
                pressure_event['payload']['summary'] = 'f' * 5000
                pressure_event['payload']['scores'][0]['reason'] = 'r' * 8000
                encoded = json.dumps(pressure_event, sort_keys=True, separators=(',', ':')).encode()
                self.assertLessEqual(len(encoded), 16 * 1024)
                request = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
                           'params': {'name': 'laya_feedback', 'arguments': pressure_event}}
                response = subprocess.run([str(BINARY), 'mcp'], input=json.dumps(request) + '\n',
                    env={**os.environ, 'LAYA_WORKBENCH_DIR': str(root), 'LAYA_PORT': '0',
                         'LAYA_PYTHON': str(ROOT / 'tests/fixtures/workbench_worker.py')},
                    text=True, capture_output=True, timeout=20, check=True)
                tool = json.loads(response.stdout)['result']
                status = 'error' if tool.get('isError') else tool['structuredContent']['status']
                self.assertIn(status, ('error', 'stored', 'queued_local'), tool)
                if status == 'error':
                    failure = json.dumps(tool).lower()
                    self.assertTrue(any(text in failure for text in ('disk is full', 'no space left', 'os error 28', 'unable to open database file')), tool)
                feedback_results.append((pressure_event['event_id'], status, hashlib.sha256(encoded).hexdigest()))
                pressure_events.append(pressure_event)
            # These are bounded, expected test data, not user evidence.
            (mount / 'filler.bin').unlink()
            service.close()
            service = Service(root)
            for _ in range(150):
                if service.rpc('status')['outbox']['pending_snapshots'] == 0:
                    break
                time.sleep(0.1)
            with closing(sqlite3.connect(root / 'laya.sqlite3')) as db:
                self.assertEqual(db.execute("SELECT payload_json,event_hash FROM feedback_events WHERE event_id='initial-score'").fetchone(), initial)
                self.assertEqual(db.execute("SELECT value,phase FROM feedback_scores WHERE event_id='initial-score'").fetchone(), (1, 'initial'))
                for request_id, status, _ in results:
                    if status in ('stored', 'queued_local'):
                        row = db.execute('SELECT result_json FROM decisions WHERE id=?', (request_id,)).fetchone()
                        self.assertIsNotNone(row, (request_id, status))
                        self.assertIsNotNone(row[0], (request_id, status))
                for event_id, status, expected_hash in feedback_results:
                    if status in ('stored', 'queued_local'):
                        self.assertEqual(db.execute('SELECT event_hash FROM feedback_events WHERE event_id=?', (event_id,)).fetchone(), (expected_hash,))
            replay = service.rpc('feedback', event)
            self.assertEqual(replay['payload_hash'], receipt['payload_hash'])
            for pressure_event, (_, _, expected_hash) in zip(pressure_events, feedback_results):
                recovered_feedback = service.rpc('feedback', pressure_event)
                self.assertEqual(recovered_feedback['status'], 'stored')
                self.assertEqual(recovered_feedback['payload_hash'], expected_hash)
                self.assertEqual(service.rpc('feedback', pressure_event)['payload_hash'], expected_hash)
            recovered = service.rpc('predict', {'state': 'after recovery', 'advisor': {'models': []}}, 'recovered')
            self.assertEqual(recovered['meta']['recording_status'], 'stored')
            print(json.dumps({'test': 'real_enospc', 'volume_bytes': stats.f_blocks * stats.f_frsize,
                              'filler_bytes': filled, 'recording_statuses': [status for _, status, _ in results],
                              'write_probe_errno': probe_errno, 'persistence_errors': failures,
                              'feedback_statuses': [status for _, status, _ in feedback_results],
                              'initial_score_preserved': True, 'recovered': True}))
        finally:
            try:
                if verified_mount and mount.stat().st_dev != directory.stat().st_dev:
                    (mount / 'filler.bin').unlink(missing_ok=True)
                if service is not None:
                    try:
                        service.close()
                    except Exception:
                        if service.process.poll() is None:
                            service.process.kill()
                            service.process.wait(timeout=5)
                        service.process.stdout.close()
                        service.process.stderr.close()
                        raise
            finally:
                if attached:
                    try:
                        if service is not None and service.process.poll() is None:
                            raise RuntimeError('Owned service termination is unconfirmed')
                        exact_device = image_entity(image, mount)
                        run('/usr/bin/hdiutil', 'detach', exact_device)
                        attached = False
                    except Exception:
                        print(f'Could not detach verified test image; preserved {directory}', file=sys.stderr)
                        raise
                if not attached:
                    shutil.rmtree(directory)


if __name__ == '__main__':
    unittest.main()
