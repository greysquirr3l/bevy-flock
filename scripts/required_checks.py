#!/usr/bin/env python3
"""The required status checks on `main`, derived from the workflows that report them.

GitHub matches a required status check by its exact name. A name that stops being reported --
because a workflow was renamed, a job was added, or a name embedded a value that later moved --
becomes a required check that never reports, and that blocks every merge from then on with
nothing on screen to say why.

This script closes that gap in three ways:

- It computes the set of check names the pull-request workflows will report, the way GitHub
  names them, including the matrix expansion.
- It compares that set against `.github/branch-protection.json`, the file that is sent to
  GitHub. Drift in either direction is an error: a name in the list that no workflow reports
  is a dead merge button, and a name a workflow reports that is not in the list is a check
  that gates nothing.
- It refuses the shapes it cannot translate faithfully rather than guessing, because a guessed
  list is wrong in a way nobody notices until a pull request cannot merge.

Usage:

    python3 scripts/required_checks.py --check
    python3 scripts/required_checks.py --update
    python3 scripts/required_checks.py --explain
    python3 scripts/required_checks.py --print-payload
    python3 scripts/required_checks.py --self-test

To apply the result to GitHub:

    python3 scripts/required_checks.py --print-payload \
        | gh api --method PUT repos/greysquirr3l/bevy-flock/branches/main/protection --input -
"""

from __future__ import annotations

import argparse
import itertools
import json
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

WORKFLOW_DIR = Path(".github/workflows")
PAYLOAD_PATH = Path(".github/branch-protection.json")

#: A context name that carries a version stops matching the day the version moves. The MSRV and
#: the compatibility pin both move, so a name that embeds either of them is a future deadlock.
VERSION_IN_NAME = re.compile(r"\d+\.\d+\.\d+")

#: The expression GitHub substitutes into a job name.
EXPRESSION = re.compile(r"\$\{\{\s*([^}]+?)\s*\}\}")

#: Jobs that report on a pull request without gating one, each with the reason it is excluded.
#: An unexplained exclusion is indistinguishable from a bug, so every one of them says why.
EXCLUDED_JOBS = {
    ("ci.yml", "release"): (
        "The release gate only runs on a tag ref, so on a pull request it reports `skipped`. "
        "Requiring it would add a name to the list without adding a signal."
    ),
}


class Refusal(Exception):
    """A workflow shape this script will not guess at."""


@dataclass(frozen=True)
class Check:
    """One check run a workflow reports on a pull request."""

    workflow: str
    job_id: str
    name: str


def load_yaml(path: Path) -> dict:
    """Parse a YAML document, or explain that PyYAML is missing."""
    try:
        import yaml
    except ImportError:
        sys.exit("required_checks: PyYAML is required (pip install pyyaml)")
    try:
        return yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    except yaml.YAMLError as error:  # pragma: no cover - malformed workflow
        sys.exit(f"required_checks: {path}: {error}")


def trigger_block(workflow: dict) -> dict:
    """The `on:` block.

    PyYAML reads the key `on` as the boolean True, because YAML 1.1 says `on` is a boolean, so
    the block is looked up under both spellings.
    """
    block = workflow.get("on")
    if block is None:
        block = workflow.get(True)
    return block if isinstance(block, dict) else {}


def pull_request_filter(workflow: dict) -> dict | None:
    """The `pull_request` trigger's filter, or None when the workflow does not run on pull requests."""
    block = trigger_block(workflow)
    if "pull_request" not in block:
        return None
    value = block["pull_request"]
    return value if isinstance(value, dict) else {}


def render_value(value: object) -> str:
    """A matrix value as GitHub writes it into a check name."""
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def matrix_combinations(matrix: dict, where: str) -> list[dict]:
    """Every combination a matrix produces, with `exclude` applied and `include` refused.

    `include` is refused because it can add combinations, alter existing ones, or both, and
    which of those happened is not readable from the matrix alone.
    """
    if "include" in matrix:
        raise Refusal(
            f"{where}: a matrix with `include:` cannot be translated, because `include` can add "
            "combinations or change them and the file does not say which. Put the matrix value "
            "in the job name and list the combinations here instead."
        )
    keys = [key for key in matrix if key != "exclude"]
    if not keys:
        raise Refusal(f"{where}: a matrix with no keys produces no combinations.")
    if len(keys) > 1:
        raise Refusal(
            f"{where}: the matrix has more than one key ({', '.join(keys)}), and GitHub's name for a "
            "matrix job with several keys is not derivable from the file alone. Reference every "
            "matrix key in the job name, or split the matrix."
        )
    values = matrix[keys[0]]
    if not isinstance(values, list) or not values:
        raise Refusal(f"{where}: the matrix key {keys[0]!r} has no values.")
    excludes = matrix.get("exclude") or []
    combinations = [{keys[0]: value} for value in values]
    return [
        combination
        for combination in combinations
        if not any(_matches_exclude(combination, exclude) for exclude in excludes)
    ]


def _matches_exclude(combination: dict, exclude: object) -> bool:
    """Whether an `exclude` entry removes a combination. An entry names a subset of the keys."""
    if not isinstance(exclude, dict):
        return False
    return all(combination.get(key) == value for key, value in exclude.items())


def render_name(job_name: str, combination: dict, where: str) -> str:
    """The check name GitHub reports for one matrix combination.

    A name that references a matrix key is substituted into it. A name that does not gets the
    matrix value appended, and an empty value is appended as nothing at all: GitHub reports
    `Feature powerset` for the empty combination, not `Feature powerset ()`.
    """
    rendered = job_name
    substituted = False
    for key, value in combination.items():
        token = re.compile(r"\$\{\{\s*" + re.escape(f"matrix.{key}") + r"\s*\}\}")
        if token.search(rendered):
            rendered = token.sub(lambda _match, text=render_value(value): text, rendered)
            substituted = True
    if substituted:
        leftover = EXPRESSION.search(rendered)
        if leftover:
            raise Refusal(
                f"{where}: the job name {job_name!r} still contains {leftover.group(0)!r} after the "
                "matrix values were substituted, so its check name cannot be derived."
            )
        return rendered
    value = render_value(next(iter(combination.values())))
    return job_name if value == "" else f"{job_name} ({value})"


def workflow_checks(path: Path) -> list[Check]:
    """Every check the workflow reports on a pull request."""
    workflow = load_yaml(path)
    pull_request = pull_request_filter(workflow)
    if pull_request is None:
        return []
    for key in ("paths", "paths-ignore"):
        if key in pull_request:
            raise Refusal(
                f"{path}: the `pull_request` trigger filters on `{key}`, so the workflow does not run "
                "on every pull request and its checks do not report. A required check that never "
                "reports blocks every merge. Move the filter onto the job's steps instead, so the "
                "job always runs and always reports a result."
            )
    checks: list[Check] = []
    for job_id, job in (workflow.get("jobs") or {}).items():
        condition = job.get("if")
        if condition is not None and "refs/tags" not in str(condition):
            raise Refusal(
                f"{path}: the job {job_id!r} has an `if:` condition that this script cannot resolve "
                f"({str(condition)!r}), so it cannot tell whether the job reports on a pull request."
            )
        if condition is not None and (path.name, job_id) not in EXCLUDED_JOBS:
            raise Refusal(
                f"{path}: the job {job_id!r} only runs on a tag ref, so it does not gate a pull "
                "request. List it in EXCLUDED_JOBS with the reason, so the exclusion is deliberate."
            )
        if (path.name, job_id) in EXCLUDED_JOBS:
            continue
        job_name = job.get("name") or job_id
        matrix = (job.get("strategy") or {}).get("matrix")
        if matrix is None:
            checks.append(Check(path.name, job_id, job_name))
            continue
        where = f"{path}: the job {job_id!r}"
        for combination in matrix_combinations(matrix, where):
            checks.append(Check(path.name, job_id, render_name(job_name, combination, where)))
    return checks


def workflow_paths() -> list[Path]:
    """Every workflow file, in a stable order."""
    return sorted(
        path for path in WORKFLOW_DIR.iterdir() if path.suffix in (".yml", ".yaml")
    )


def generated_checks() -> list[Check]:
    """Every check the pull-request workflows report, in a stable order."""
    checks: list[Check] = []
    for path in workflow_paths():
        checks.extend(workflow_checks(path))
    return checks


def context_names(checks: list[Check]) -> list[str]:
    """The check names, sorted and without duplicates."""
    return sorted({check.name for check in checks})


def read_payload() -> dict:
    """The branch protection request body, which is also the payload sent to GitHub."""
    try:
        return json.loads(PAYLOAD_PATH.read_text(encoding="utf-8"))
    except FileNotFoundError:
        sys.exit(f"required_checks: {PAYLOAD_PATH} is missing.")
    except json.JSONDecodeError as error:
        sys.exit(f"required_checks: {PAYLOAD_PATH}: {error}")


def write_payload(payload: dict) -> None:
    """Write the payload back, with the contexts in the place the API expects them."""
    PAYLOAD_PATH.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def payload_contexts(payload: dict) -> list[str]:
    """The required contexts the payload carries."""
    checks = payload.get("required_status_checks") or {}
    return list(checks.get("contexts") or [])


def check() -> int:
    """Compare the committed list against the workflows. Returns the exit status."""
    try:
        checks = generated_checks()
    except Refusal as refusal:
        print(f"required_checks: {refusal}")
        return 1
    generated = context_names(checks)
    payload = read_payload()
    committed = payload_contexts(payload)

    problems = []
    missing = sorted(set(generated) - set(committed))
    stale = sorted(set(committed) - set(generated))
    if missing:
        problems.append(
            "these checks are reported but not required:\n"
            + "\n".join(f"  + {name}" for name in missing)
        )
    if stale:
        problems.append(
            "these checks are required but no workflow reports them, so a pull request could "
            "never merge:\n" + "\n".join(f"  - {name}" for name in stale)
        )
    versioned = [name for name in committed if VERSION_IN_NAME.search(name)]
    if versioned:
        problems.append(
            "these required check names carry a version, so they stop matching when the version "
            "moves:\n" + "\n".join(f"  ! {name}" for name in versioned)
        )

    reviews = payload.get("required_pull_request_reviews") or {}
    if not isinstance(reviews.get("required_approving_review_count"), int):
        problems.append("the payload has no integer `required_approving_review_count`.")
    if reviews.get("required_approving_review_count", 0) < 1:
        problems.append("the payload requires no approving review.")
    if not reviews.get("require_code_owner_reviews"):
        problems.append("the payload does not require a code owner review, so `.github/CODEOWNERS` has no effect.")
    if not (payload.get("required_status_checks") or {}).get("strict"):
        problems.append("the payload does not require the branch to be up to date before merging.")

    if problems:
        print("required_checks: the required checks and the workflows disagree.")
        for problem in problems:
            print(problem)
        print(f"required_checks: run `python3 scripts/required_checks.py --update` and commit the result.")
        return 1
    print(f"required_checks: {len(committed)} required checks match the workflows.")
    return 0


def update() -> int:
    """Write the computed contexts into the payload."""
    try:
        checks = generated_checks()
    except Refusal as refusal:
        print(f"required_checks: {refusal}")
        return 1
    payload = read_payload()
    payload.setdefault("required_status_checks", {})["contexts"] = context_names(checks)
    write_payload(payload)
    print(f"required_checks: wrote {len(context_names(checks))} contexts to {PAYLOAD_PATH}.")
    return 0


def explain() -> int:
    """Print what each workflow reports, what is excluded, and why."""
    try:
        checks = generated_checks()
    except Refusal as refusal:
        print(f"required_checks: {refusal}")
        return 1
    payload = read_payload()
    committed = set(payload_contexts(payload))
    for path in workflow_paths():
        names = [check.name for check in checks if check.workflow == path.name]
        print(f"{path.name}")
        if not names:
            print("  (does not run on pull requests)")
        for name in names:
            marker = "required" if name in committed else "not required"
            print(f"  {name}  [{marker}]")
    for (workflow, job_id), reason in sorted(EXCLUDED_JOBS.items()):
        print(f"{workflow}: the job {job_id!r} is excluded. {reason}")
    print(f"\n{len(committed)} required checks are in {PAYLOAD_PATH}.")
    return 0


def print_payload() -> int:
    """Print the request body for GitHub's update-branch-protection endpoint."""
    json.dump(read_payload(), sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


def self_test() -> int:
    """Prove the parts that translate a workflow into a check name."""
    failures: list[str] = []

    def expect(label: str, actual: object, wanted: object) -> None:
        if actual != wanted:
            failures.append(f"{label}: got {actual!r}, wanted {wanted!r}")

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        workflows = root / "workflows"
        workflows.mkdir()

        def workflow(text: str) -> Path:
            path = workflows / "sample.yml"
            path.write_text(text, encoding="utf-8")
            return path

        expect(
            "a matrix value is substituted into the name",
            [check.name for check in workflow_checks(workflow(
                "on:\n  pull_request:\njobs:\n  t:\n    name: Test (${{ matrix.os }})\n"
                "    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-latest]\n"
            ))],
            ["Test (ubuntu-latest)", "Test (macos-latest)"],
        )
        expect(
            "a name with no matrix reference gets the value appended",
            [check.name for check in workflow_checks(workflow(
                "on:\n  pull_request:\njobs:\n  f:\n    name: Feature powerset\n"
                "    strategy:\n      matrix:\n        features: ['', 'memory', 'navmesh,simd']\n"
            ))],
            ["Feature powerset", "Feature powerset (memory)", "Feature powerset (navmesh,simd)"],
        )
        expect(
            "a workflow without a pull_request trigger reports nothing",
            [check.name for check in workflow_checks(workflow(
                "on:\n  workflow_run:\n    workflows: [CI]\njobs:\n  t:\n    name: tag\n"
            ))],
            [],
        )

        for label, text in (
            (
                "a path filter is refused",
                "on:\n  pull_request:\n    paths: ['site/**']\njobs:\n  b:\n    name: build\n",
            ),
            (
                "a matrix with include is refused",
                "on:\n  pull_request:\njobs:\n  t:\n    name: t\n    strategy:\n      matrix:\n"
                "        os: [a]\n        include:\n          - os: b\n",
            ),
            (
                "a matrix with two keys is refused",
                "on:\n  pull_request:\njobs:\n  t:\n    name: t\n    strategy:\n      matrix:\n"
                "        os: [a]\n        python: [3]\n",
            ),
            (
                "a job condition this script cannot resolve is refused",
                "on:\n  pull_request:\njobs:\n  t:\n    if: github.actor == 'someone'\n    name: t\n",
            ),
            (
                "a tag-gated job that is not on the exclusion list is refused",
                "on:\n  pull_request:\njobs:\n  t:\n    if: startsWith(github.ref, 'refs/tags/v')\n"
                "    name: t\n",
            ),
        ):
            try:
                workflow_checks(workflow(text))
            except Refusal:
                continue
            failures.append(f"{label}: it was accepted")

        # The excluded-job table is keyed by file name, so the sample workflow has to be named
        # `ci.yml` for the exclusion to apply.
        excluded = workflows / "ci.yml"
        excluded.write_text(
            "on:\n  pull_request:\njobs:\n  release:\n    if: startsWith(github.ref, 'refs/tags/v')\n",
            encoding="utf-8",
        )
        expect(
            "the release gate is excluded by name",
            [check.name for check in workflow_checks(excluded)],
            [],
        )

    expect("a version in a name is found", bool(VERSION_IN_NAME.search("MSRV (1.97.1)")), True)
    expect("an ADR number is not a version", bool(VERSION_IN_NAME.search("Cell-size re-open gate (ADR 0031)")), False)
    expect("a wasm target is not a version", bool(VERSION_IN_NAME.search("wasm32 build")), False)

    if failures:
        for failure in failures:
            print(f"required_checks: self-test failed: {failure}")
        return 1
    print("required_checks: self-test passed.")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="compare the required list with the workflows")
    parser.add_argument("--update", action="store_true", help="write the computed list into the payload")
    parser.add_argument("--explain", action="store_true", help="print what each workflow reports")
    parser.add_argument("--print-payload", action="store_true", help="print the request body for GitHub")
    parser.add_argument("--self-test", action="store_true", help="prove the name translation")
    arguments = parser.parse_args(argv)

    if arguments.self_test:
        return self_test()
    if arguments.update:
        return update()
    if arguments.explain:
        return explain()
    if arguments.print_payload:
        return print_payload()
    return check()


if __name__ == "__main__":
    sys.exit(main())