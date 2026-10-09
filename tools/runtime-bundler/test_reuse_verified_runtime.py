# The Compukters Developers
#
# Copyright 2026 Vsevolod Petrov (lazyhat)
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Protect source-run provenance and release tag validation."""

from pathlib import Path
from urllib.parse import parse_qs, urlsplit
import tempfile
import unittest

from reuse_verified_runtime import find_source_run, validate_identity

REPOSITORY = "owner/runtime"
SHA = "a" * 40


def run_record(run_id=10):
    return {"id": run_id, "head_sha": SHA, "event": "push", "status": "completed",
            "conclusion": "success", "head_repository": {"full_name": REPOSITORY}}


def artifact_record(run_id=10):
    return {"name": f"runtime-verified-{SHA}", "expired": False,
            "workflow_run": {"id": run_id, "head_sha": SHA}}


class ReuseTests(unittest.TestCase):
    def select(self, runs=None, artifacts=None, current=99):
        runs = [run_record()] if runs is None else runs
        artifacts = [artifact_record()] if artifacts is None else artifacts
        def api(endpoint):
            if "/workflows/ci.yml/runs?" in endpoint:
                self.assertIn(f"head_sha={SHA}", endpoint)
                self.assertIn("event=push", endpoint)
                return {"workflow_runs": runs}
            return {"artifacts": artifacts}
        return find_source_run(api, REPOSITORY, SHA, current)

    def test_reuses_successful_push_of_exact_commit(self):
        self.assertEqual("10", self.select())

    def test_rejects_current_wrong_commit_other_repo_and_unverified_runs(self):
        mutations = [dict(head_sha="b" * 40), dict(event="pull_request"),
                     dict(event="workflow_dispatch"), dict(status="in_progress"),
                     dict(conclusion="failure"), dict(conclusion="cancelled"),
                     dict(head_repository=None),
                     dict(head_repository={"full_name": "fork/runtime"})]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.assertEqual("", self.select(runs=[dict(run_record(), **mutation)]))
        self.assertEqual("", self.select(current=10))

    def test_missing_expired_ambiguous_or_wrong_provenance_artifacts_are_not_reused(self):
        cases = [[], [dict(artifact_record(), expired=True)],
                 [dict(artifact_record(), name="runtime-linux")],
                 [artifact_record(), artifact_record()],
                 [dict(artifact_record(), workflow_run={"id": 10, "head_sha": "b" * 40})],
                 [dict(artifact_record(), workflow_run={"id": 11, "head_sha": SHA})],
                 [dict(artifact_record(), workflow_run=None)]]
        for artifacts in cases:
            with self.subTest(artifacts=artifacts):
                self.assertEqual("", self.select(artifacts=artifacts))

    def test_finds_older_valid_run_after_expired_artifact(self):
        def api(endpoint):
            if "/workflows/" in endpoint:
                return {"workflow_runs": [run_record(11), run_record(10)]}
            return {"artifacts": [dict(artifact_record(11), expired=True)] if "/runs/11/" in endpoint
                    else [artifact_record(10)]}
        self.assertEqual("10", find_source_run(api, REPOSITORY, SHA, 99))

    def test_paginates_runs_and_artifacts(self):
        def api(endpoint):
            if "/workflows/" in endpoint:
                return {"workflow_runs": [run_record(99)] * 100 if parse_qs(urlsplit(endpoint).query)["page"] == ["1"] else [run_record()]}
            return {"artifacts": [dict(artifact_record(), name="other")] * 100 if parse_qs(urlsplit(endpoint).query)["page"] == ["1"]
                    else [artifact_record()]}
        self.assertEqual("10", find_source_run(api, REPOSITORY, SHA, 99))

    def test_api_errors_fail_instead_of_silently_rebuilding(self):
        def api(_endpoint):
            raise RuntimeError("API unavailable")
        with self.assertRaisesRegex(RuntimeError, "API unavailable"):
            find_source_run(api, REPOSITORY, SHA, 99)

    def test_tag_identity_is_checked_before_reuse(self):
        with tempfile.TemporaryDirectory() as directory:
            version = Path(directory) / "runtime-version.toml"
            version.write_text('version = "0.21.1"\n')
            validate_identity(version, "")
            validate_identity(version, "v0.21.1")
            for invalid in ["main", "v0.21.0", "v0.22.0", "v0.21.1\n"]:
                with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                    validate_identity(version, invalid)


if __name__ == "__main__":
    unittest.main()
