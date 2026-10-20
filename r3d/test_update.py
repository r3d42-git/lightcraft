"""Exercise update replay in disposable Git repositories, never the live checkout."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("prepare-update.py")


class UpdateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="r3d-update-test-")
        self.root = Path(self.temp.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Local Test")
        self.git("config", "user.email", "local-test@example.invalid")
        (self.root / "base.txt").write_text("base\n")
        self.git("add", "base.txt")
        self.git("commit", "-qm", "upstream base")
        self.base = self.git("rev-parse", "HEAD")
        self.git("switch", "-qc", "own")
        (self.root / "r3d").mkdir()
        shutil.copyfile(SCRIPT, self.root / "r3d/prepare-update.py")
        (self.root / "r3d/upstream-base.json").write_text(json.dumps({"commit": self.base}))
        (self.root / "own.txt").write_text("own\n")
        self.git("add", "r3d/prepare-update.py", "r3d/upstream-base.json", "own.txt")
        self.git("commit", "-qm", "own extension")
        self.own = self.git("rev-parse", "HEAD")
        self.git("switch", "-qc", "upstream-next", self.base)
        (self.root / "new-upstream.txt").write_text("new upstream\n")
        self.git("add", "new-upstream.txt")
        self.git("commit", "-qm", "upstream next")
        self.git("switch", "-q", "own")
        self.destination = Path(self.temp.name) / "integrated"

    def tearDown(self):
        self.temp.cleanup()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], text=True).strip()

    def run_update(self, *args):
        return subprocess.run(
            [sys.executable, str(self.root / "r3d/prepare-update.py"), "upstream-next",
             "--destination", str(self.destination), *args],
            text=True, capture_output=True,
        )

    def test_preview_is_read_only_and_prepare_replays_on_new_base(self):
        preview = self.run_update()
        self.assertEqual(preview.returncode, 0, preview.stderr)
        self.assertFalse(self.destination.exists())
        self.assertEqual(self.git("rev-parse", "HEAD"), self.own)
        prepared = self.run_update("--prepare")
        self.assertEqual(prepared.returncode, 0, prepared.stderr)
        self.assertEqual((self.destination / "new-upstream.txt").read_text(), "new upstream\n")
        self.assertEqual((self.destination / "own.txt").read_text(), "own\n")
        self.assertEqual(self.git("rev-parse", "HEAD"), self.own)
        self.assertEqual(self.git("status", "--porcelain"), "")
        manifest = json.loads((self.destination / "r3d/upstream-base.json").read_text())
        self.assertEqual(manifest["commit"], self.git("rev-parse", "upstream-next"))

    def test_dirty_source_is_refused_before_creating_worktree(self):
        (self.root / "own.txt").write_text("uncommitted\n")
        result = self.run_update("--prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not clean", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_merge_only_changes_cannot_be_silently_dropped(self):
        self.git("switch", "-qc", "side")
        (self.root / "side.txt").write_text("side\n")
        self.git("add", "side.txt")
        self.git("commit", "-qm", "side extension")
        self.git("switch", "-q", "own")
        self.git("merge", "--no-ff", "--no-commit", "side")
        (self.root / "merge-only.txt").write_text("integration fix\n")
        self.git("add", "merge-only.txt")
        self.git("commit", "-qm", "merge with integration change")
        result = self.run_update("--prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Merge commits", result.stderr)
        self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
