"""Real CLI port behavior, isolated from the installed workbench database."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import unittest
import urllib.request
from test_service_failures import BINARY


@unittest.skipUnless(BINARY.exists(), "Build the Rust binary first")
class DashboardPortTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='laya-port-', dir='/private/tmp')
        self.root = Path(self.directory.name)
        self.env = {**os.environ, 'LAYA_WORKBENCH_DIR': str(self.root)}
        self.env.pop('LAYA_PORT', None)

    def command(self, *args):
        return subprocess.run([str(BINARY), *args], env=self.env, capture_output=True, text=True, timeout=15)

    def tearDown(self):
        self.command('stop')
        for _ in range(100):
            if not (self.root / 'service.sock').exists():
                break
            time.sleep(.02)
        self.directory.cleanup()

    def test_default_fixed_port_and_favicon(self):
        with socket.socket() as probe:
            try:
                probe.bind(('127.0.0.1', 18686))
            except OSError:
                self.skipTest('default port already occupied by another service')
        result = self.command('dashboard', '--no-open')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith('http://127.0.0.1:18686/#pair='))
        with urllib.request.urlopen('http://127.0.0.1:18686/favicon.svg') as response:
            self.assertIn('image/svg+xml', response.headers['Content-Type'])
            self.assertIn(b'Oh My Laya', response.read())

    def test_override_reuse_and_mismatch_never_restart(self):
        self.env['LAYA_PORT'] = 'invalid-but-cli-wins'
        result = self.command('dashboard', '--port', '0', '--no-open')
        self.assertEqual(result.returncode, 0, result.stderr)
        initial = json.loads(self.command('status').stdout)['service']
        port = initial['port']
        self.assertGreater(port, 0)
        self.assertEqual(self.command('dashboard', f'--port={port}', '--no-open').returncode, 0)
        mismatch = self.command('dashboard', '--port', str(1 if port != 1 else 2), '--no-open')
        self.assertNotEqual(mismatch.returncode, 0)
        self.assertIn('service already running', mismatch.stderr)
        self.assertEqual(json.loads(self.command('status').stdout)['service']['instance'], initial['instance'])

    def test_collision_is_actionable_and_does_not_fall_back(self):
        with socket.socket() as occupied:
            occupied.bind(('127.0.0.1', 0))
            occupied.listen()
            port = occupied.getsockname()[1]
            result = self.command('dashboard', '--port', str(port), '--no-open')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('--port <free-port>', result.stderr)
            self.assertFalse((self.root / 'service.json').exists())

    def test_environment_override_and_invalid_input(self):
        self.env['LAYA_PORT'] = '0'
        self.assertEqual(self.command('dashboard', '--no-open').returncode, 0)
        self.assertGreater(json.loads(self.command('status').stdout)['service']['port'], 0)
        for args in [('--port',), ('--port=-1',), ('--port=65536',), ('--port=1', '--port=2')]:
            self.assertNotEqual(self.command('dashboard', *args, '--no-open').returncode, 0)
