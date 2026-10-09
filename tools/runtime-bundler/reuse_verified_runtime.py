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

"""Select a verified Runtime artifact from a successful push of this exact commit."""

import json
import os
from pathlib import Path
import subprocess
from urllib.parse import urlencode


def github_api(endpoint):
    return json.loads(subprocess.check_output(["gh", "api", endpoint], text=True))


def pages(api, endpoint, key, parameters=None):
    page = 1
    while True:
        query = dict(parameters or {}, per_page=100, page=page)
        items = api(f"{endpoint}?{urlencode(query)}")[key]
        yield from items
        if len(items) < 100:
            return
        page += 1


def find_source_run(api, repository, sha, current_run):
    artifact_name = f"runtime-verified-{sha}"
    runs = pages(api, f"repos/{repository}/actions/workflows/ci.yml/runs", "workflow_runs",
                 {"head_sha": sha, "event": "push", "status": "success"})
    for run in runs:
        if (str(run["id"]) == str(current_run) or run.get("head_sha") != sha
                or run.get("event") != "push" or run.get("status") != "completed"
                or run.get("conclusion") != "success"
                or (run.get("head_repository") or {}).get("full_name") != repository):
            continue
        artifacts = list(pages(api, f"repos/{repository}/actions/runs/{run['id']}/artifacts", "artifacts"))
        matching = [artifact for artifact in artifacts
                    if artifact.get("name") == artifact_name and not artifact.get("expired", True)]
        if len(matching) != 1:
            continue
        provenance = matching[0].get("workflow_run") or {}
        if provenance.get("id") != run["id"] or provenance.get("head_sha") != sha:
            continue
        return str(run["id"])
    return ""


def validate_identity(version_file, requested_tag):
    version = json.loads(Path(version_file).read_text().split("=", 1)[1])
    if requested_tag and requested_tag != f"v{version}":
        raise ValueError(f"Runtime tag {requested_tag!r} does not match v{version}")


def main():
    validate_identity("runtime-version.toml", os.environ.get("RUNTIME_TAG", ""))
    run = find_source_run(github_api, os.environ["GITHUB_REPOSITORY"],
                          os.environ["GITHUB_SHA"], os.environ["GITHUB_RUN_ID"])
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"source_run={run}\n")
    print(f"Reuse verified Runtime from run {run}" if run else "No reusable verified Runtime; run full verification")


if __name__ == "__main__":
    main()
