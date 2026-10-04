"""Build the real Rust metadata script across Git tag and checkout changes."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(shutil.which("cargo") and shutil.which("git"), "Cargo and Git required")
class BuildVersion(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rust version ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.env = dict(os.environ)
        self.env["CARGO_TARGET_DIR"] = str(self.root / "target")
        self.env["GIT_CEILING_DIRECTORIES"] = str(self.root)
        shutil.copy(ROOT / "build.rs", self.source)
        shutil.copy(ROOT / "Cargo.lock", self.source)
        (self.source / "Cargo.toml").write_text(
            '[package]\nname = "version-fixture"\nversion = "0.0.0"\nedition = "2021"\n'
            '[build-dependencies]\nvergen-gitcl = "=1.0.8"\n')
        (self.source / "src").mkdir()
        (self.source / "src/main.rs").write_text(
            'fn main() { println!("{}", option_env!("VERGEN_GIT_DESCRIBE").unwrap_or("missing")); }\n')
        (self.source / ".gitignore").write_text('Cargo.lock\n')
        self.git("init", "-q")
        self.git("config", "user.name", "Version Test")
        self.git("config", "user.email", "version@example.invalid")
        self.git("add", ".")
        self.git("commit", "-qm", "test: initialize source")

    def run_command(self, *args):
        result = subprocess.run(args, cwd=self.source, env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout.strip()

    def git(self, *args):
        return self.run_command("git", *args)

    def version(self):
        return self.run_command("cargo", "run", "--offline", "--quiet")

    def check_transitions(self):
        revision = self.git("rev-parse", "--short", "HEAD")
        self.assertEqual(self.version(), revision)
        self.git("tag", "unrelated-tag")
        self.assertEqual(self.version(), revision)
        self.git("tag", "-a", "v0.2.0", "-m", "test release")
        self.assertEqual(self.version(), "v0.2.0")
        self.git("pack-refs", "--all")
        self.assertEqual(self.version(), "v0.2.0")
        self.git("tag", "-d", "v0.2.0")
        self.assertEqual(self.version(), revision)
        self.git("tag", "-a", "v0.2.0", "-m", "test release")
        self.assertEqual(self.version(), "v0.2.0")
        main = self.source / "src/main.rs"
        main.write_text(main.read_text() + "// local edit\n")
        self.assertEqual(self.version(), "v0.2.0-dirty")
        self.git("add", "src/main.rs")
        self.git("commit", "-qm", "test: advance checkout")
        revision = self.git("rev-parse", "--short", "HEAD")
        self.assertEqual(self.version(), "v0.2.0-1-g" + revision)

    def test_tag_changes_refresh_embedded_version(self):
        self.check_transitions()

    def test_worktree_uses_common_tags(self):
        checkout = self.root / "worktree"
        self.git("worktree", "add", "--detach", str(checkout))
        shutil.copy(self.source / "Cargo.lock", checkout)
        self.source = checkout
        self.check_transitions()

    def test_source_archive_has_unknown_version(self):
        shutil.rmtree(self.source / ".git")
        self.assertEqual(self.version(), "VERGEN_IDEMPOTENT_OUTPUT")
