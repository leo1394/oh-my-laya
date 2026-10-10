import subprocess
import sys
from pathlib import Path
import unittest


class VersionTests(unittest.TestCase):
    def test_metadata_and_tag_match_single_version(self):
        root = Path(__file__).resolve().parents[1]
        version = (root / 'VERSION.txt').read_text().strip()
        script = root / 'tools/version.py'
        result = subprocess.run([sys.executable, str(script), '--tag', 'v' + version], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), version)
        bad = subprocess.run([sys.executable, str(script), '--tag', 'v0.0.0'], capture_output=True, text=True)
        self.assertNotEqual(bad.returncode, 0)
