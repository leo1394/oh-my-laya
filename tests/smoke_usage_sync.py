"""Native counters -> reconciliation -> MCP receipts -> dashboard, in isolation."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
from http.cookiejar import CookieJar
from urllib.parse import urlparse, parse_qs
from urllib.request import Request, build_opener, HTTPCookieProcessor

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('LAYA_TEST_BINARY', ROOT / 'target/debug/laya'))


def main():
    with tempfile.TemporaryDirectory(prefix='laya-usage-', dir='/private/tmp') as temporary:
        directory = Path(temporary)
        data = directory / 'data'
        env = {**os.environ, 'LAYA_WORKBENCH_DIR': str(data), 'LAYA_PORT': '0',
               'LAYA_NATIVE_USAGE_SYNC': '0', 'LAYA_PYTHON': str(ROOT / 'tests/fixtures/workbench_worker.py'),
               'PYTHONPATH': str(ROOT / 'src')}
        process = subprocess.Popen([str(BINARY), 'service'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        def rpc(method, params=None):
            with socket.socket(socket.AF_UNIX) as channel:
                channel.settimeout(10)
                channel.connect(str(data / 'service.sock'))
                channel.sendall(json.dumps({'protocol_version': 1, 'request_id': str(time.time_ns()), 'method': method, 'params': params or {}}).encode() + b'\n')
                response = json.loads(channel.makefile('rb').readline())
            assert 'error' not in response, response
            return response['result']
        try:
            for _ in range(100):
                if (data / 'service.json').exists():
                    break
                time.sleep(.05)
            origin = 'http://127.0.0.1:' + str(json.loads((data / 'service.json').read_text())['port'])
            opener = build_opener(HTTPCookieProcessor(CookieJar()))
            def api(path, body=None, method=None):
                request = Request(origin + '/api/v1/' + path, data=json.dumps(body).encode() if body is not None else None,
                                  method=method, headers={'Origin': origin, 'Content-Type': 'application/json'})
                return json.loads(opener.open(request, timeout=10).read())
            code = parse_qs(urlparse(rpc('pair')['url']).fragment)['pair'][0]
            api('pair', {'code': code})
            api('settings', {'recording_enabled': True}, 'PATCH')
            decision = rpc('predict', {'state': 'fixture usage', 'questions': {'risk': {}}})
            identity = decision['meta']['decision_id']
            home = directory / 'codex'
            sessions = home / 'sessions'
            sessions.mkdir(parents=True)
            root = [
                {'type': 'session_meta', 'payload': {'id': 'root', 'source': 'vscode'}},
                {'type': 'turn_context', 'payload': {'turn_id': 'turn', 'model': 'model', 'effort': 'medium'}},
                {'type': 'response_item', 'payload': {'type': 'function_call', 'name': 'mcp__oh_my_laya__laya_tell_me', 'call_id': 'call'}},
                {'type': 'response_item', 'payload': {'type': 'function_call_output', 'call_id': 'call', 'output': json.dumps({'structuredContent': decision})}},
            ]
            child = [
                {'type': 'session_meta', 'payload': {'id': 'child', 'source': {'subagent': {'thread_spawn': {'parent_thread_id': 'root'}}}}},
                {'type': 'turn_context', 'payload': {'turn_id': 'child-turn', 'model': 'small', 'effort': 'low'}},
            ]
            def usage(thread, turn, response, amount, cumulative):
                return {'type': 'token_usage_record', 'timestamp': '2026-10-12T04:00:00Z', 'payload': {
                    'thread_id': thread, 'turn_id': turn, 'root_turn_id': 'turn', 'response_id': response,
                    'usage': {'total_tokens': amount, 'input_tokens': amount-10, 'output_tokens': 10},
                    'turn_token_usage': {'total_tokens': cumulative, 'input_tokens': cumulative-(20 if response == 'root-2' else 10), 'output_tokens': 20 if response == 'root-2' else 10}}}
            root.append(usage('root', 'turn', 'root-1', 100, 100))
            root.append({'type': 'event_msg', 'timestamp': '2020-01-01T00:00:00Z', 'payload': {'type': 'task_complete', 'turn_id': 'turn'}})
            child.append(usage('child', 'child-turn', 'child-1', 50, 50))
            def write():
                for name, records in [('root', root), ('child', child)]:
                    (sessions / (name + '.jsonl')).write_text(''.join(json.dumps(record) + '\n' for record in records))
            write()
            command = [sys.executable, '-m', 'laya_tell_me.usage_sync', '--workbench', str(data), '--codex-home', str(home),
                       '--skill', str(ROOT / 'skills/alpha-squad-coding-craft/skills/alpha-squad-coding-craft'), '--laya', str(BINARY), '--deliver']
            def sync():
                return json.loads(subprocess.check_output(command, env=env, text=True, timeout=30))
            # Recovery from a prepared ledger must still deliver usage before
            # manifests, despite canonical JSON sorting manifest IDs first.
            subprocess.check_output(command[:-1], env=env, text=True, timeout=30)
            assert sync()['stored_tasks'] == 1
            tokens = api('overview')['dashboard']['tokens']
            assert tokens['recorded_total'] == tokens['scenario']['actual_total'] == 150, tokens
            assert tokens['scenario']['saved']['central'] is not None
            count = len(api('decisions/' + identity)['feedback'])
            assert sync()['stored_tasks'] == 1
            assert len(api('decisions/' + identity)['feedback']) == count
            root.append(usage('root', 'turn', 'root-2', 200, 300))
            write()
            assert sync()['stored_tasks'] == 1
            tokens = api('overview')['dashboard']['tokens']
            assert tokens['recorded_total'] == tokens['scenario']['actual_total'] == 350, tokens
            api('settings', {'recording_enabled': False}, 'PATCH')
            assert sync()['status'] == 'disabled'
            assert api('overview')['dashboard']['tokens']['recorded_total'] == 350
            api('settings', {'recording_enabled': True}, 'PATCH')
            api('decisions/' + identity, method='DELETE')
            assert sync()['matched_turns'] == 0
            print('PASS: root + child tokens, matched estimate, idempotency, growing checkpoints, consent, deletion')
        finally:
            try:
                rpc('stop')
                process.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                process.terminate()
                process.wait(timeout=5)
            process.stderr.close()


if __name__ == '__main__':
    main()
