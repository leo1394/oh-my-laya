from pathlib import Path
import unittest

from laya_agent_bridge.download import is_valid_model, sha256_file


class DownloadTests(unittest.TestCase):
    def test_sha256_file(self):
        from tempfile import TemporaryDirectory

        with TemporaryDirectory() as directory:
            path = Path(directory) / "data"
            path.write_bytes(b"abc")
            self.assertEqual(
                sha256_file(path),
                "ba7816bf8f01cfea414140de5dae2223"
                "b00361a396177a9cb410ff61f20015ad",
            )

    def test_incomplete_model_is_invalid(self):
        from tempfile import TemporaryDirectory

        with TemporaryDirectory() as directory:
            self.assertFalse(is_valid_model(Path(directory), "unused"))


if __name__ == "__main__":
    unittest.main()
