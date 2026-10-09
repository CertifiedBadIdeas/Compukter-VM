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

"""Exercise real checkpoint exchange between two revisions of the same ABI."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def run(arguments, root, **kwargs):
    return subprocess.run(arguments, cwd=root, check=True, **kwargs)


def build_fixture(root, destination, environment):
    output = run(
        ["cargo", "test", "-p", "compukter-vm", "--test", "computer_machine",
         "--no-run", "--message-format=json", "--locked", "--offline"],
        root, env=environment, stdout=subprocess.PIPE, text=True,
    )
    executables = [item["executable"] for item in map(json.loads, output.stdout.splitlines())
                   if item.get("reason") == "compiler-artifact" and item.get("executable")
                   and item["target"]["name"] == "computer_machine"]
    if len(executables) != 1:
        raise RuntimeError(f"Expected one checkpoint fixture executable: {executables}")
    shutil.copy2(executables[0], destination)


def exchange(producer, producer_version, consumer, consumer_version, directory):
    directory.mkdir()
    for executable, version, phase in [(producer, producer_version, "save"),
                                        (consumer, consumer_version, "restore")]:
        environment = dict(os.environ,
                           COMPUKTERS_CHECKPOINT_PROCESS_ROOT=str(directory),
                           COMPUKTERS_CHECKPOINT_PROCESS_PHASE=phase,
                           COMPUKTERS_CHECKPOINT_PROCESS_VERSION=version)
        output = run(
            [str(executable), "--exact", "checkpoint_process_fixture", "--ignored", "--nocapture"],
            directory, env=environment, stdout=subprocess.PIPE, text=True,
        )
        print(output.stdout, flush=True)
        if "test result: ok. 1 passed;" not in output.stdout:
            raise RuntimeError(f"Checkpoint fixture did not run for {version}: {output.stdout}")
    print(f"Checkpoint restored: {producer_version} -> {consumer_version}", flush=True)


def main():
    source = Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix="compukter-revision-compatibility-") as temporary:
        temporary = Path(temporary)
        repository = temporary / "repository"
        repository.mkdir()
        tracked = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=source)
        for name in tracked.decode().split("\0"):
            if not name:
                continue
            destination = repository / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / name, destination)
        # Keep the cache isolated: xtask embeds its repository path at compile time.
        environment = dict(os.environ, CARGO_TARGET_DIR=str(temporary / "target"))
        version = json.loads((repository / "runtime-version.toml").read_text().split("=", 1)[1])
        suffix = ".exe" if os.name == "nt" else ""
        original = temporary / ("original" + suffix)
        revised = temporary / ("revised" + suffix)
        run(["cargo", "xtask", "check"], repository, env=environment)
        build_fixture(repository, original, environment)
        # This is a build fixture, not a production release/version mutation.
        # Keep it offline without bypassing the publication gate of cargo xtask bump.
        abi, revision = version.rsplit(".", 1)
        next_version = f"{abi}.{int(revision) + 1}"
        (repository / "runtime-version.toml").write_text(f'version = "{next_version}"\n')
        manifest = repository / "Cargo.toml"
        old_version = f'version = "{version}"'
        contents = manifest.read_text()
        if contents.count(old_version) != 1:
            raise RuntimeError("Expected exactly one canonical workspace version in build fixture")
        manifest.write_text(contents.replace(old_version, f'version = "{next_version}"'))
        run(["cargo", "metadata", "--format-version", "1", "--offline"], repository,
            env=environment, stdout=subprocess.DEVNULL)
        run(["cargo", "xtask", "check"], repository, env=environment)
        build_fixture(repository, revised, environment)
        exchange(original, version, revised, next_version, temporary / "forward")
        exchange(revised, next_version, original, version, temporary / "backward")


if __name__ == "__main__":
    main()
