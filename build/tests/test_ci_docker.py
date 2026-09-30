import importlib.util
import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).resolve().parents[1] / "ci-docker.py"
SPEC = importlib.util.spec_from_file_location("ci_docker", MODULE_PATH)
assert SPEC and SPEC.loader
ci_docker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci_docker)


class DockerVolumeTranslationTests(unittest.TestCase):
    mounts = [
        {
            "Type": "volume",
            "Name": "runner-workspace",
            "Source": "/daemon/volumes/runner-workspace/_data",
            "Destination": "/workspace/checkout",
        }
    ]

    def test_translates_workspace_path_and_preserves_spaces_and_mode(self):
        translated = ci_docker.translate_volume(
            "/workspace/checkout/.ci build/target:/work:ro", self.mounts
        )
        self.assertEqual(
            translated,
            "/daemon/volumes/runner-workspace/_data/.ci build/target:/work:ro",
        )

    def test_keeps_named_volumes_unchanged(self):
        self.assertEqual(
            ci_docker.translate_volume("cache:/cache", self.mounts), "cache:/cache"
        )

    def test_rejects_absolute_sources_outside_named_workspace(self):
        with self.assertRaisesRegex(ValueError, "outside a named job workspace"):
            ci_docker.translate_volume("/tmp/unknown:/work", self.mounts)

    def test_prefers_the_most_specific_mount(self):
        mounts = self.mounts + [
            {
                "Type": "volume",
                "Name": "nested",
                "Source": "/daemon/volumes/nested/_data",
                "Destination": "/workspace/checkout/.ci-build",
            }
        ]
        self.assertEqual(
            ci_docker.translate_volume(
                "/workspace/checkout/.ci-build/target:/work", mounts
            ),
            "/daemon/volumes/nested/_data/target:/work",
        )

    def test_build_is_forwarded_without_inspection(self):
        completed = subprocess.CompletedProcess([], 0)
        with patch.object(ci_docker.subprocess, "run", return_value=completed) as run:
            self.assertEqual(ci_docker.main(["build", "."]), 0)
        run.assert_called_once_with(["docker", "build", "."], check=False)

    def test_reports_signal_exit_using_shell_convention(self):
        completed = subprocess.CompletedProcess([], -15)
        with patch.object(ci_docker.subprocess, "run", return_value=completed):
            self.assertEqual(ci_docker.main(["build", "."]), 143)

    def test_run_translates_each_volume_and_preserves_other_args(self):
        with patch.object(ci_docker, "workspace_mounts", return_value=self.mounts):
            completed = subprocess.CompletedProcess([], 0)
            with patch.object(
                ci_docker.subprocess, "run", return_value=completed
            ) as run:
                self.assertEqual(
                    ci_docker.main(
                        [
                            "run",
                            "--rm",
                            "-v",
                            "/workspace/checkout/qemu:/src:ro",
                            "--volume=/workspace/checkout/.ci-build:/work",
                            "builder",
                        ]
                    ),
                    0,
                )
        run.assert_called_once_with(
            [
                "docker",
                "run",
                "--rm",
                "-v",
                "/daemon/volumes/runner-workspace/_data/qemu:/src:ro",
                "--volume=/daemon/volumes/runner-workspace/_data/.ci-build:/work",
                "builder",
            ],
            check=False,
        )


if __name__ == "__main__":
    unittest.main()
