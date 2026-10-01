#!/usr/bin/env python3

import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


class ReleaseVersionTests(unittest.TestCase):
    def test_updates_manifest_lock_and_public_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "1.0.0"\n')
            (root / "Cargo.lock").write_text('[[package]]\nname = "fixture"\nversion = "1.0.0"\n')
            (root / "VERSION").write_text("1.0.0\n")
            result = subprocess.run(
                [
                    "python3",
                    str(ROOT / "scripts/set-release-version.py"),
                    "2026.07.21.12",
                    "--root",
                    str(root),
                    "--package",
                    "fixture",
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.stdout.strip(), "2026.7.21-12")
            self.assertIn('version = "2026.7.21-12"', (root / "Cargo.toml").read_text())
            self.assertIn('version = "2026.7.21-12"', (root / "Cargo.lock").read_text())
            self.assertEqual((root / "VERSION").read_text(), "2026.07.21.12\n")


if __name__ == "__main__":
    unittest.main()
