import json
from pathlib import Path
import tempfile
import unittest
import sqlite3
from types import SimpleNamespace
from unittest.mock import Mock

from laya_tell_me.usage_sync import discover, prepare_ledger, reconcile, reconcile_snapshot, scan


class UsageSyncTests(unittest.TestCase):
    def test_active_turn_and_explicit_squad_ledger_keep_ownership(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = SimpleNamespace(workbench=root)
            lifecycle = Mock()
            found = {('root', 'turn'): ['decision']}
            result = reconcile_snapshot(args, lifecycle, {'decision'}, {}, root, found, {}, {'root': None}, {}, {})
            self.assertEqual(result['pending_tasks'], 1)
            lifecycle.collect.assert_not_called()
            (root / 'squad-usage/task.json').write_text('{}')
            lifecycle.load.return_value = {'root_thread_id': 'root', 'root_turn_ids': ['turn']}
            result = reconcile_snapshot(args, lifecycle, {'decision'}, {}, root, found, {}, {'root': None}, {}, {('root', 'turn'): 0})
            self.assertEqual(result['skipped_tasks'], 1)
            lifecycle.collect.assert_not_called()

    def test_numeric_completion_time_uses_native_event_timestamp(self):
        with tempfile.TemporaryDirectory() as directory:
            records = self.records() + [{'type': 'event_msg', 'timestamp': '2020-01-01T00:00:00Z',
                'payload': {'type': 'task_complete', 'turn_id': 'turn', 'completed_at': 1577836800}}]
            completed = scan(self.history(directory, records), {'decision'})[4]
            self.assertEqual(completed[('root', 'turn')], 1577836800)

    def test_disabled_collection_does_not_create_a_ledger(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with sqlite3.connect(root / 'laya.sqlite3') as db:
                db.execute('CREATE TABLE settings(singleton INTEGER, value_json TEXT)')
                db.execute('INSERT INTO settings VALUES(1, ?)', (json.dumps({'recording_enabled': False}),))
            self.assertEqual(reconcile(SimpleNamespace(workbench=root))['status'], 'disabled')
            self.assertFalse((root / 'squad-usage').exists())

    def test_snapshot_keeps_numeric_evidence_not_prompts(self):
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryDirectory() as output:
            records = self.records()
            records[0]['payload']['base_instructions'] = 'PRIVATE PROMPT'
            records[1]['payload']['instructions'] = 'PRIVATE PROMPT'
            home = self.history(directory, records)
            scan(home, {'decision'}, Path(output))
            text = next((Path(output) / 'sessions').glob('*.jsonl')).read_text()
            self.assertNotIn('PRIVATE PROMPT', text)
            self.assertNotIn('state', text)
            self.assertIn('medium', text)

    def history(self, directory, records):
        folder = Path(directory) / 'sessions'
        folder.mkdir(exist_ok=True)
        path = folder / 'root.jsonl'
        path.write_text('\n'.join(json.dumps(record) for record in records) + '\n')
        return Path(directory)

    def records(self):
        return [
            {'type': 'session_meta', 'payload': {'id': 'root', 'source': 'vscode'}},
            {'type': 'turn_context', 'payload': {'turn_id': 'turn', 'model': 'model', 'effort': 'medium'}},
            {'type': 'response_item', 'payload': {'type': 'custom_tool_call', 'name': 'exec', 'call_id': 'call',
             'input': 'text(await tools.mcp__oh_my_laya__laya_tell_me({state:"task"}));'}},
            {'type': 'response_item', 'payload': {'type': 'custom_tool_call_output', 'call_id': 'call',
             'output': [{'type': 'input_text', 'text': json.dumps({'structuredContent': {
                 'meta': {'decision_id': 'decision', 'recording_status': 'stored'}}})}]}},
        ]

    def test_native_receipt_is_bound_to_call_turn_not_latest_turn(self):
        with tempfile.TemporaryDirectory() as directory:
            records = self.records()
            records.insert(3, {'type': 'turn_context', 'payload': {'turn_id': 'later'}})
            found = discover(self.history(directory, records), {'decision'})
            self.assertEqual(found, {('root', 'turn'): ['decision']})

    def test_no_quoted_or_unpaired_decisions_or_guardian_history(self):
        with tempfile.TemporaryDirectory() as directory:
            records = self.records()
            records[2]['payload']['input'] = 'text("mcp__oh_my_laya__laya_tell_me");'
            self.assertEqual(discover(self.history(directory, records), {'decision'}), {})
            records = self.records()
            records[0]['payload']['source'] = {'subagent': {'other': 'guardian'}}
            self.assertEqual(discover(self.history(directory, records), {'decision'}), {})
            self.assertEqual(discover(self.history(directory, self.records()), {'unrelated'}), {})

    def test_whole_turn_accounting_includes_children_once_and_reuses_anchor(self):
        report = {'root_thread_id': 'root', 'root_turn_ids': ['turn'], 'segments': [
            {'thread_id': 'root', 'turn_id': 'turn', 'root_turn_id': 'turn', 'usage_stream_id': 'root-stream'},
            {'thread_id': 'child', 'turn_id': 'child-turn', 'root_turn_id': 'turn', 'usage_stream_id': 'child-stream'},
        ]}
        configs = {('root', 'turn'): {('model', 'medium')}, ('child', 'child-turn'): {('small', 'low')}}
        ledger = prepare_ledger(Path('/tmp/codex'), report, 'decision', configs, {})
        self.assertEqual(len(ledger['registrations']), 2)
        self.assertEqual({v['role'] for v in ledger['registrations'].values()}, {'root', 'child'})
        self.assertEqual({v['decision_id'] for v in ledger['registrations'].values()}, {'decision'})
        self.assertIsNone(prepare_ledger(Path('/tmp/codex'), report, 'decision', configs, {'child-stream': [('other', 'manual')]}))
        configs[('child', 'child-turn')].add(('other', 'high'))
        self.assertIsNone(prepare_ledger(Path('/tmp/codex'), report, 'decision', configs, {}))
