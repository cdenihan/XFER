#!/usr/bin/env python3

import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


class RenderInstallerTests(unittest.TestCase):
    def test_renders_identity_without_markers(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            subprocess.run(
                [
                    "python3",
                    str(ROOT / "scripts/render-installers.py"),
                    "--binary",
                    "xfer",
                    "--display-name",
                    "XFER",
                    "--repository",
                    "cdenihan/XFER",
                    "--environment-prefix",
                    "XFER",
                    "--output",
                    str(output),
                ],
                check=True,
            )
            shell = (output / "install.sh").read_text()
            powershell = (output / "install.ps1").read_text()
            self.assertIn('PROGRAM="xfer"', shell)
            self.assertIn('DEFAULT_REPOSITORY="cdenihan/XFER"', shell)
            self.assertIn('$Program = "xfer"', powershell)
            self.assertIn(
                "[IO.File]::Replace($Source, $Destination, $backup)", powershell
            )
            self.assertNotIn(
                "[IO.File]::Replace($staging, $destination, $null)", powershell
            )
            self.assertNotIn("__BINARY", shell + powershell)

    def test_rejects_injection_characters(self) -> None:
        result = subprocess.run(
            [
                "python3",
                str(ROOT / "scripts/render-installers.py"),
                "--binary",
                "bad;name",
                "--display-name",
                "Bad",
                "--repository",
                "owner/repo",
                "--environment-prefix",
                "BAD",
                "--output",
                "/tmp/unused-xfer-test",
            ],
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
