"""Small shell integration checks; no Rust build or macOS configuration changes."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class ScratchEnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.root = self.base / "Build Volume"
        self.root.mkdir()
        self.bin = self.base / "bin"
        self.bin.mkdir()
        df = self.bin / "df"
        df.write_text('''#!/usr/bin/env python3
import os, sys
from pathlib import Path
p = Path(sys.argv[-1]).resolve()
root = Path(os.environ['R3D_SCRATCH_ROOT']).resolve()
device = '/dev/scratch' if p == root or root in p.parents else '/dev/system'
if os.environ.get('MOCK_MODE') == 'home-device' and p == Path.home().resolve():
    device = '/dev/scratch'
print('Filesystem 512-blocks Used Available Capacity Mounted on')
print(device + ' 100 0 100 0% /mock')
''')
        df.chmod(0o755)
        diskutil = self.bin / "diskutil"
        diskutil.write_text('''#!/usr/bin/env python3
import os, plistlib, sys
from pathlib import Path
root = Path(os.environ['R3D_SCRATCH_ROOT']).resolve()
inside = sys.argv[-1] == '/dev/scratch'
mode = os.environ.get('MOCK_MODE', '')
device = 'disk9s1' if inside and mode != 'same-device' else 'disk1s1'
mount = str(root) if mode != 'ordinary-directory' else str(root.parent)
sys.stdout.buffer.write(plistlib.dumps(dict(DeviceIdentifier=device,
    MountPoint=mount, FilesystemType='hfs' if mode == 'not-apfs' else 'apfs')))
''')
        diskutil.chmod(0o755)
        tmutil = self.bin / "tmutil"
        tmutil.write_text('''#!/usr/bin/env bash
if [[ "${MOCK_MODE:-}" == included ]]; then
  echo '[Included] mock'
else
  echo '[Excluded] mock'
fi
''')
        tmutil.chmod(0o755)
        self.env = dict(os.environ)
        for key in ("CARGO_TARGET_DIR", "R3D_BACKUP_STAGING", "R3D_TMPDIR", "DIST"):
            self.env.pop(key, None)
        self.env.update(R3D_SCRATCH_ROOT=str(self.root),
                        PATH=f"{self.bin}:{self.env['PATH']}")
        self.script = Path(__file__).resolve().parent / "dev-env.sh"

    def source(self, **overrides):
        return subprocess.run(["bash", "-c",
            'source "$1" || exit $?; printf "%s\\n" "$CARGO_TARGET_DIR" "$TMPDIR"',
            "test", str(self.script)], env=dict(self.env, **overrides),
            text=True, capture_output=True)

    def test_defaults_and_explicit_target(self):
        result = self.source()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(str(self.root / "lightcraft/target-"), result.stdout)
        target = str(self.root / "custom-target")
        result = self.source(CARGO_TARGET_DIR=target)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith(target + "\n"))

    def test_unavailable_or_unsafe_volume_fails(self):
        for mode in ("ordinary-directory", "same-device", "home-device", "not-apfs", "included"):
            with self.subTest(mode=mode):
                result = self.source(MOCK_MODE=mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("scratch check failed", result.stderr)
        result = self.source(R3D_SCRATCH_ROOT=str(self.base / "missing"))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.base / "missing").exists())

    def test_outside_and_symlink_escape_fail(self):
        escape = self.root / "escape"
        escape.symlink_to(self.base)
        for path in (str(self.base / "target"), str(escape / "target"), "relative"):
            with self.subTest(path=path):
                self.assertNotEqual(self.source(CARGO_TARGET_DIR=path).returncode, 0)


if __name__ == "__main__":
    unittest.main()
