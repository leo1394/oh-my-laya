"""Reconcile recorded Laya receipts with Codex-native turn usage, never task text.

A decision is an accounting anchor, not an assertion that it selected the model
for every descendant. No assignment, outcome, score or training label is made.
Delivery uses the existing Squad producer and Laya MCP consent/idempotency gates.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import time
import tempfile
from types import SimpleNamespace
from datetime import datetime


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def records(path):
    with path.open(encoding='utf-8') as stream:
        for line in stream:
            try:
                yield json.loads(line)
            except ValueError:
                if line.endswith('\n'):
                    raise


def receipt_ids(value, known):
    """Only structured tool results, never substring matches in conversation text."""
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except ValueError:
            return []
    if isinstance(value, list):
        return [identity for part in value if isinstance(part, dict)
                and part.get('type') in ('input_text', 'text')
                for identity in receipt_ids(part.get('text'), known)]
    if not isinstance(value, dict) or value.get('isError'):
        return []
    result = value.get('structuredContent')
    if isinstance(result, dict):
        meta = result.get('meta', {})
        identity = meta.get('decision_id')
        return [identity] if identity in known and meta.get('recording_status') == 'stored' else []
    return receipt_ids(value.get('content'), known)


def scan(home, known, snapshot=None):
    found, configurations, parents, usage, completed = {}, {}, {}, {}, {}
    for folder in ('sessions', 'archived_sessions'):
        for path in sorted((home / folder).rglob('*.jsonl')):
            try:
                iterator = records(path)
                first = next(iterator, {})
                meta = first.get('payload', {})
                if first.get('type') != 'session_meta':
                    continue
                thread, source = meta.get('id'), meta.get('source')
                if not thread:
                    continue
                spawn = source.get('subagent', {}).get('thread_spawn') if isinstance(source, dict) else None
                if isinstance(source, dict) and not spawn:
                    continue  # Guardian/review copies are not original tool executions.
                parents[thread] = spawn.get('parent_thread_id') if spawn else None
                evidence = [{'type': 'session_meta', 'payload': {'id': thread, 'source': source}}] if snapshot is not None else None
                turn, calls = None, {}
                for record in iterator:
                    payload = record.get('payload', {})
                    if record.get('type') == 'turn_context':
                        if evidence is not None:
                            evidence.append({'type': 'turn_context', 'payload': {key: payload.get(key) for key in ('turn_id', 'model', 'effort')}})
                        turn = payload.get('turn_id')
                        if turn:
                            configurations.setdefault((thread, turn), set()).add((payload.get('model'), payload.get('effort')))
                    elif record.get('type') == 'token_usage_record':
                        if evidence is not None:
                            evidence.append(record)
                        if payload.get('thread_id') == thread:
                            usage.setdefault(payload.get('root_turn_id'), set()).add(thread)
                    elif record.get('type') == 'event_msg' and payload.get('type') == 'task_complete':
                        stamp = payload.get('completed_at') or record.get('timestamp')
                        if type(stamp) in (int, float):
                            stamp = record.get('timestamp')  # Native completed_at is epoch seconds on newer hosts.
                        if isinstance(stamp, str):
                            completed[(thread, payload.get('turn_id'))] = datetime.fromisoformat(stamp.replace('Z', '+00:00')).timestamp()
                    elif record.get('type') == 'response_item':
                        kind, name = payload.get('type'), payload.get('name', '')
                        if kind in ('function_call', 'custom_tool_call'):
                            code = payload.get('input', payload.get('arguments', ''))
                            native = name == 'mcp__oh_my_laya__laya_tell_me' or (
                                name == 'exec' and isinstance(code, str) and re.search(
                                    r'\bawait\s+tools\.mcp__oh_my_laya__laya_tell_me\s*\(', code))
                            if native and turn:
                                calls[payload.get('call_id')] = turn
                        elif kind in ('function_call_output', 'custom_tool_call_output'):
                            call_turn = calls.get(payload.get('call_id'))
                            if call_turn:
                                for identity in receipt_ids(payload.get('output'), known):
                                    identities = found.setdefault((thread, call_turn), [])
                                    if identity not in identities:
                                        identities.append(identity)
                if evidence is not None:
                    target = snapshot / 'sessions' / (digest(str(path)) + '.jsonl')
                    target.parent.mkdir(exist_ok=True)
                    target.write_text(''.join(json.dumps(item) + '\n' for item in evidence))
            except (OSError, ValueError, AttributeError, TypeError):
                continue  # No output inferred from unreadable or incompatible histories.
    return found, configurations, parents, usage, completed


def discover(home, known):
    return scan(home, known)[0]


def prepare_ledger(home, report, anchor, configurations, existing):
    root = report['root_thread_id']
    ledger = {'version': 1, 'codex_home': str(home), 'root_thread_id': root,
              'root_turn_ids': report['root_turn_ids'], 'registrations': {}, 'events': {}}
    for segment in report['segments']:
        identity = (segment['thread_id'], segment['turn_id'])
        pairs = configurations.get(identity, set())
        if len(pairs) != 1 or not all(next(iter(pairs))):
            return None
        attempt = 'native-turn-' + digest(identity)
        prior = existing.get(segment['usage_stream_id'], [])
        if any(item != (anchor, attempt) for item in prior):
            return None  # Never double-account streams already managed by another producer.
        ledger['registrations'][attempt] = {
            'decision_id': anchor, 'root_turn_id': segment['root_turn_id'],
            'role': 'root' if segment['thread_id'] == root else 'child',
            'binding': {**{key: segment[key] for key in ('thread_id', 'turn_id', 'root_turn_id')},
                        'decision_id': anchor, 'attempt_ref': attempt, 'configuration_scope_confirmed': True},
        }
    return ledger


def reconcile(args):
    # Read-only database access is discovery only; all writes go through laya_feedback.
    database = sqlite3.connect((args.workbench / 'laya.sqlite3').as_uri() + '?mode=ro', uri=True)
    try:
        settings = json.loads(database.execute('SELECT value_json FROM settings WHERE singleton=1').fetchone()[0])
        if not settings.get('recording_enabled') or not settings.get('replay_enabled', True):
            return {'status': 'disabled', 'stored_tasks': 0}
        known = {row[0] for row in database.execute('SELECT id FROM decisions WHERE deleted_at IS NULL')}
        existing = {}
        for decision, attempt, stream in database.execute("SELECT decision_id,attempt_ref,json_extract(payload_json,'$.usage_stream_id') FROM feedback_events WHERE kind='usage'"):
            if stream:
                existing.setdefault(stream, []).append((decision, attempt))
    finally:
        database.close()
    sys.path.insert(0, str(args.skill / 'scripts'))
    import laya_usage_lifecycle as lifecycle
    # A short-lived numeric-only snapshot prevents repeatedly parsing large image
    # and conversation records for every turn. Original native records are intact.
    with tempfile.TemporaryDirectory(prefix='laya-native-usage-') as temporary:
        snapshot = Path(temporary)
        found, configurations, parents, usage, completed = scan(args.codex_home, known, snapshot)
        return reconcile_snapshot(args, lifecycle, known, existing, snapshot, found, configurations, parents, usage, completed)


def reconcile_snapshot(args, lifecycle, known, existing, snapshot, found, configurations, parents, usage, completed):
    result = {'status': 'ok', 'matched_turns': len(found), 'stored_tasks': 0, 'pending_tasks': 0, 'skipped_tasks': 0}
    directory = args.workbench / 'squad-usage' / 'native-reconcile'
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    managed = set()
    for path in (args.workbench / 'squad-usage').rglob('*.json'):
        if directory in path.parents:
            continue
        try:
            ledger = lifecycle.load(path)
            managed.update((ledger['root_thread_id'], turn) for turn in ledger['root_turn_ids'])
        except (OSError, ValueError, KeyError, TypeError):
            continue
    for (root, turn), decisions in found.items():
        if parents.get(root) is not None:
            continue  # Descendants are counted only through their verified root ancestry.
        if (root, turn) in managed:
            result['skipped_tasks'] += 1
            continue  # Explicit Squad ledgers retain ownership, including queued feedback.
        if time.time() - completed.get((root, turn), time.time()) < 120:
            result['pending_tasks'] += 1
            continue  # Give the task's own finalizer time to persist its native checkpoint.
        related = {root}
        while True:
            expanded = related | {child for child, parent in parents.items() if parent in related}
            if expanded == related:
                break
            related = expanded
        count = len((usage.get(turn, set()) & related) - {root})
        report = lifecycle.collect(snapshot, root, {turn}, count)
        if report['status'] != 'ok':
            result['pending_tasks'] += 1
            continue
        path = directory / (digest([root, turn]) + '.json')
        with lifecycle.locked(path):
            prior = lifecycle.load(path) if path.exists() else None
            anchor = next(iter(prior['registrations'].values()))['decision_id'] if prior else decisions[0]
            if anchor not in known:
                result['skipped_tasks'] += 1
                continue
            ledger = prepare_ledger(args.codex_home, report, anchor, configurations, existing)
            if ledger is None:
                result['skipped_tasks'] += 1
                continue
            if prior:
                ledger['events'] = prior['events']
                # Only verified new segments may extend the same cumulative task ledger.
                if any(ledger['registrations'].get(key) != value for key, value in prior['registrations'].items()):
                    result['pending_tasks'] += 1
                    continue
            lifecycle.save(path, ledger)
            ledger['codex_home'] = str(snapshot)
            try:
                outcome = lifecycle.checkpoint(path, ledger, SimpleNamespace(
                    deliver=False, laya=str(args.laya), timeout=10))
                if args.deliver and outcome.get('estimate', {}).get('status') == 'ok':
                    # JSON ledgers sort keys; dependency manifests must follow
                    # their usage receipts even after restart/recovery.
                    ledger['events'] = dict(sorted(ledger['events'].items(),
                        key=lambda item: item[1]['event']['kind'] == 'run_manifest'))
                    lifecycle.delivery(path, ledger, str(args.laya), 10)
                    outcome = lifecycle.summary(ledger)
            finally:
                ledger['codex_home'] = str(args.codex_home)
                lifecycle.save(path, ledger)
            complete = outcome.get('delivery_complete') and outcome.get('estimate', {}).get('status') == 'ok'
            result['stored_tasks' if complete else 'pending_tasks'] += 1
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--workbench', type=Path, required=True)
    parser.add_argument('--codex-home', type=Path, default=Path(os.environ.get('CODEX_HOME', Path.home() / '.codex')))
    parser.add_argument('--skill', type=Path)
    parser.add_argument('--laya', type=Path, required=True)
    parser.add_argument('--deliver', action='store_true')
    args = parser.parse_args()
    if args.skill is None:
        options = [args.codex_home / 'skills/alpha-squad-coding-craft', Path.home() / '.agents/skills/alpha-squad-coding-craft']
        args.skill = next((path for path in options if (path / 'scripts/laya_usage_lifecycle.py').is_file()), options[0])
    try:
        result = reconcile(args)
    except (OSError, ValueError, KeyError, TypeError, ImportError, sqlite3.Error) as error:
        result = {'status': 'unavailable', 'error': str(error)}
    result['observed_at'] = int(time.time())
    print(json.dumps(result))
    return 0 if result['status'] in ('ok', 'disabled') else 2


if __name__ == '__main__':
    raise SystemExit(main())
