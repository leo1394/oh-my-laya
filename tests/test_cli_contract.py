import os
from pathlib import Path
import subprocess
import re
import shutil
import tempfile
import unittest

from laya_tell_me.runtime_install import install_cli_resources, _validate_binary


ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('LAYA_TEST_BINARY', ROOT / 'target/debug/laya'))


@unittest.skipUnless(BINARY.is_file(), 'build the CLI first')
class CliContractTests(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run([str(BINARY), *args], text=True, capture_output=True, timeout=5,
                              env={**os.environ, 'LAYA_WORKBENCH_DIR': '/does/not/exist', 'LAYA_PYTHON': '/missing'})

    def test_version_help_and_errors_are_offline(self):
        version = self.run_cli('version')
        self.assertEqual(version.returncode, 0, version.stderr)
        self.assertEqual(version.stdout, self.run_cli('--version').stdout)
        self.assertRegex(version.stdout, r'^laya version \d+\.\d+\.\d+(?:-dev)? \(\d{4}-\d{2}-\d{2}\)\nhttps://github.com/leo1394/oh-my-laya\n$')
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / 'laya'
            shutil.copy2(BINARY, artifact)
            _validate_binary(artifact)
        self.assertEqual(self.run_cli('help').stdout, self.run_cli('--help').stdout)
        for command in ('dashboard', 'status', 'stop', 'service', 'mcp', 'snake', 'version', 'completion'):
            result = self.run_cli('help', command)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('Usage:', result.stdout)
            self.assertEqual(result.stdout, self.run_cli(command, '--help').stdout)
        for args, suggestion in [(('versoin',), 'version'), (('dashboard', '--no-opne'), '--no-open')]:
            result = self.run_cli(*args)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, '')
            self.assertIn('Did you mean', result.stderr)
            self.assertIn(suggestion, result.stderr)
        self.assertNotEqual(self.run_cli('status', '--bad').returncode, 0)

    def test_install_manual_and_behavioral_bash_completion(self):
        with tempfile.TemporaryDirectory(prefix='laya cli ') as directory:
            prefix = Path(directory)
            install_cli_resources(BINARY, prefix)
            manual = prefix / 'share/man/man1/laya.1'
            rendered = subprocess.run(['mandoc', '-Tutf8', str(manual)], capture_output=True, text=True, check=True)
            self.assertIn('SYNOPSIS', re.sub('.\x08', '', rendered.stdout))
            found = subprocess.check_output(['man', '-w', 'laya'], env={**os.environ, 'MANPATH': str(prefix / 'share/man') + ':'}, text=True)
            self.assertIn('laya.1', found)
            completion = prefix / 'share/bash-completion/completions/laya'
            def candidates(words, index):
                script = 'source "$1"; shift; COMP_WORDS=("$@"); COMP_CWORD=' + str(index) + '; _laya; printf "%s\\n" "${COMPREPLY[@]}"'
                return subprocess.check_output(['/bin/bash', '-c', script, 'bash', str(completion), *words], text=True, cwd=prefix).splitlines()
            self.assertIn('dashboard', candidates(['laya', 'da'], 1))
            self.assertIn('dashboard', candidates(['laya', 'help', 'da'], 2))
            self.assertEqual(candidates(['laya', 'completion', 'zs'], 2), ['zsh'])
            self.assertEqual(candidates(['laya', 'snake', '--prompt', 'de'], 3), ['detailed'])
            (prefix / 'a file.jsonl').touch()
            self.assertEqual(candidates(['laya', 'snake', '--record', 'a '], 3), ['a file.jsonl'])
            self.assertNotIn('--port', candidates(['laya', 'dashboard', '--port', '123', ''], 4))
            self.assertEqual(candidates(['laya', 'dashboard', '--port', ''], 3), [''])
            self.assertEqual(candidates(['laya', 'dashboard', '--', ''], 3), [''])
            subprocess.run(['zsh', '-n', str(prefix / 'share/zsh/site-functions/_laya')], check=True)
