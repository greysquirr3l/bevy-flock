#!/usr/bin/env python3
"""Run this repository's CI jobs locally, read out of `ci.yml` rather than copied from it.

A second copy of the gates would drift the first time someone edited one of them, so this script
translates the workflow instead: it expands each job's matrix, substitutes `${{ matrix.* }}` into the
`run:` steps, and executes them in order with `bash`. Steps that provision the runner (`uses:`) are
reported and skipped, because the whole point of running locally is that the machine is already set
up. Anything the translator cannot faithfully reproduce is a `--check` failure, not a silent skip:
`release` is gated on a tag ref and `deny` is an action with no `run:` step, and both are listed
below with their reason so the list cannot quietly grow.

Usage:

    python3 scripts/ci_local.py --list
    python3 scripts/ci_local.py --check
    python3 scripts/ci_local.py --job fmt --job scripts
    python3 scripts/ci_local.py --all --keep-going

Why it exists: CI is the gate that says `main` is healthy, and a gate behind a billing failure is no
gate at all. This is the same set of commands, runnable on any machine with the toolchain installed.
"""

from __future__ import annotations

import argparse
import itertools
import os
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
from dataclasses import dataclass, field
from pathlib import Path

WORKFLOW = Path(".github/workflows/ci.yml")

#: Jobs that cannot be reproduced as shell commands, with the reason and the flag that opts in.
UNREPRODUCIBLE = {
    "release": (
        "gated on a tag ref (`startsWith(github.ref, 'refs/tags/v')`); "
        "run it by passing --tag on a release branch"
    ),
    "deny": "an action with no `run:` step (the licence audit runs inside cargo-deny)",
}

#: Runners whose jobs can run here. A matrix that varies only `os` would otherwise run the Linux
#: commands three times and call two of them a macOS pass.
RUNNER_OS = {
    "ubuntu-latest": "linux",
    "ubuntu-24.04": "linux",
    "macos-latest": "macos",
    "windows-latest": "windows",
}

#: A step reads as this pattern when it is environment provisioning rather than a command.
EXPRESSION = re.compile(r"\$\{\{\s*([^}]+?)\s*\}\}")

BOLD = "\033[1m"
DIM = "\033[2m"
GREEN = "\033[32m"
RED = "\033[31m"
YELLOW = "\033[33m"
RESET = "\033[0m"


def colour(code: str, text: str) -> str:
    """Wrap `text` in `code` when writing to a terminal."""
    if not sys.stdout.isatty():
        return text
    return f"{code}{text}{RESET}"


@dataclass
class Step:
    """One translated step."""

    name: str
    command: str | None
    skipped: str | None = None
    cwd: Path | None = None


@dataclass
class Job:
    """A job, already expanded over its matrix."""

    name: str
    variant: str
    steps: list[Step] = field(default_factory=list)
    env: dict[str, str] = field(default_factory=dict)
    unsupported: str | None = None
    runner: str | None = None


def load_workflow(path: Path):
    """Parse the workflow, failing loudly when PyYAML is missing."""
    try:
        import yaml
    except ImportError:  # pragma: no cover - environment problem, not a code path
        sys.exit("ci_local: PyYAML is required (pip install pyyaml)")
    try:
        return yaml.safe_load(path.read_text())
    except FileNotFoundError:
        sys.exit(f"ci_local: no workflow at {path}")
    except yaml.YAMLError as error:  # pragma: no cover - malformed workflow
        sys.exit(f"ci_local: {path} is not valid YAML: {error}")


def substitute(text: str, variables: dict[str, str], unresolved: list[str]) -> str:
    """Replace `${{ matrix.* }}` and record anything else left standing."""

    def replace(match: re.Match[str]) -> str:
        inner = match.group(1).strip()
        key = inner.removeprefix("matrix.")
        if inner.startswith("matrix.") and key in variables:
            return variables[key]
        unresolved.append(match.group(0))
        return match.group(0)

    return EXPRESSION.sub(replace, text)


def expand(job_body: dict) -> list[dict[str, str]]:
    """Expand `strategy.matrix` into one variable set per run of the job."""
    matrix = (job_body.get("strategy") or {}).get("matrix")
    if not matrix:
        return [{}]
    keys = list(matrix)
    return [dict(zip(keys, values, strict=True)) for values in itertools.product(*(matrix[k] for k in keys))]


def translate(name: str, body: dict, include_tag_jobs: bool) -> list[Job]:
    """Turn a job into the shell commands a local machine would run, one job per matrix variant."""
    condition = body.get("if")
    if condition and "refs/tags" not in str(condition):
        return [
            Job(
                name=name,
                variant="",
                unsupported=f"job-level condition this script does not evaluate: {condition}",
            )
        ]
    if condition and not include_tag_jobs:
        return [Job(name=name, variant="", unsupported=f"tag-gated ({condition}); pass --tag to run")]

    translated: list[Job] = []
    for variables in expand(body):
        variant = ",".join(f"{k}={v}" for k, v in variables.items()) if variables else ""
        job = Job(name=name, variant=variant)
        unresolved: list[str] = []
        runs_on = substitute(str(body.get("runs-on", "")), variables, unresolved)
        target = RUNNER_OS.get(runs_on)
        job.runner = target
        if target is not None and target != local_os():
            job.unsupported = f"runs on {runs_on}; this machine is {local_os()}"
        for key, value in (body.get("env") or {}).items():
            job.env[key] = substitute(str(value), variables, unresolved)
        for step in body.get("steps", []):
            label = step.get("name") or step.get("uses") or "(unnamed)"
            if "uses" in step and "run" not in step:
                job.steps.append(Step(name=label, command=None, skipped=f"provisions the runner: {step['uses']}"))
                continue
            if "run" not in step:
                job.steps.append(Step(name=label, command=None, skipped="nothing to run"))
                continue
            step_condition = step.get("if")
            if step_condition and step_condition.strip() != "always()":
                job.steps.append(
                    Step(name=label, command=None, skipped=f"step condition not evaluated: {step_condition}")
                )
                continue
            command = substitute(step["run"], variables, unresolved)
            cwd = step.get("working-directory")
            job.steps.append(
                Step(
                    name=substitute(str(label), variables, unresolved),
                    command=command,
                    cwd=Path(cwd) if cwd else None,
                )
            )
        if unresolved:
            job.unsupported = "unsubstituted expression(s): " + ", ".join(sorted(set(unresolved)))
        translated.append(job)
    return translated


def benign(reason: str) -> bool:
    """True when a skip is a fact about this machine rather than a gap in the translator."""
    return reason.startswith("runs on ") or "tag-gated" in reason


def local_os() -> str:
    """This machine, in the vocabulary `RUNNER_OS` uses."""
    if sys.platform.startswith("darwin"):
        return "macos"
    if sys.platform.startswith("win"):
        return "windows"
    return "linux"


def summarise(job: Job) -> str:
    """One line describing what a job variant would do."""
    if job.unsupported:
        return colour(YELLOW, f"skip  {job.name}{f' [{job.variant}]' if job.variant else ''}: {job.unsupported}")
    commands = [step for step in job.steps if step.command]
    skipped = [step for step in job.steps if step.skipped]
    label = job.name + (f" [{job.variant}]" if job.variant else "")
    return f"{label}: {len(commands)} command(s), {len(skipped)} step(s) skipped"


def run_job(job: Job, root: Path) -> tuple[str, bool]:
    """Execute one job variant; return its label and whether it passed."""
    label = job.name + (f" [{job.variant}]" if job.variant else "")
    if job.unsupported:
        print(colour(YELLOW, f"SKIP {label}: {job.unsupported}"))
        return label, True
    environment = {**os.environ, **job.env}
    for step in job.steps:
        if step.skipped:
            print(colour(DIM, f"  - {step.name}: {step.skipped}"))
            continue
        assert step.command is not None
        where = root / step.cwd if step.cwd else root
        print(colour(DIM, f"  $ {step.command.splitlines()[0][:160]}"))
        result = subprocess.run(  # noqa: S603 - the commands come from the repository's own CI
            ["bash", "-eo", "pipefail", "-c", step.command],
            cwd=where,
            env=environment,
            check=False,
        )
        if result.returncode != 0:
            print(colour(RED, f"FAIL {label} at step `{step.name}` (exit {result.returncode})"))
            return label, False
    print(colour(GREEN, f"PASS {label}"))
    return label, True


def jobs_of(workflow: dict, include_tag_jobs: bool, wanted: list[str]) -> list[Job]:
    """Every translated job variant, honouring an optional selection."""
    selected: list[Job] = []
    for name, body in (workflow.get("jobs") or {}).items():
        if wanted and name not in wanted:
            continue
        if name in UNREPRODUCIBLE and not (include_tag_jobs and name == "release"):
            selected.append(Job(name=name, variant="", unsupported=UNREPRODUCIBLE[name]))
            continue
        selected.extend(translate(name, body, include_tag_jobs))
    return selected


def check(workflow: dict) -> int:
    """Fail when any job cannot be reproduced and is not a declared exception."""
    problems: list[str] = []
    jobs = workflow.get("jobs") or {}
    for name in jobs:
        if name in UNREPRODUCIBLE:
            continue
        for job in translate(name, jobs[name], include_tag_jobs=False):
            if job.unsupported and not benign(job.unsupported):
                problems.append(f"{name}: {job.unsupported}")
    stale = [name for name in UNREPRODUCIBLE if name not in jobs]
    for name in stale:
        problems.append(f"{name}: listed as unreproducible but the workflow no longer has this job")
    for problem in problems:
        print(colour(RED, f"problem: {problem}"))
    if problems:
        return 1
    jobs_count = len(jobs)
    variants = len(translate_all(workflow))
    print(
        f"ci_local: {jobs_count} job(s) translated into {variants} local run(s), "
        f"{len(UNREPRODUCIBLE)} declared exception(s)"
    )
    return 0


def translate_all(workflow: dict) -> list[Job]:
    """Every job variant, including the ones a run would skip, for counting."""
    return jobs_of(workflow, include_tag_jobs=True, wanted=[])


def self_test() -> int:
    """Prove the translator on fixtures, including the shapes it must refuse."""
    tmp = Path(tempfile.mkdtemp(prefix="ci-local-selftest-"))
    try:
        fixture = tmp / "ci.yml"
        fixture.write_text(
            textwrap.dedent(
                """
                jobs:
                  plain:
                    steps:
                      - uses: actions/checkout@v4
                      - run: echo plain
                  matrix:
                    strategy:
                      matrix:
                        features: ["", "memory"]
                    env:
                      FEATURES: "${{ matrix.features }}"
                    steps:
                      - run: echo "matrix ${{ matrix.features }}"
                  tag:
                    if: startsWith(github.ref, 'refs/tags/v')
                    steps:
                      - run: echo released
                  odd:
                    steps:
                      - run: echo "${{ secrets.TOKEN }}"
                """
            )
        )
        workflow = load_workflow(fixture)
        cases: list[tuple[str, bool, str]] = []

        plain = translate("plain", workflow["jobs"]["plain"], include_tag_jobs=False)
        cases.append(
            (
                "checkout is skipped, the command is kept",
                len(plain) == 1 and plain[0].steps[0].skipped is not None and plain[0].steps[1].command == "echo plain",
                f"{plain[0].steps}",
            )
        )

        matrix = translate("matrix", workflow["jobs"]["matrix"], include_tag_jobs=False)
        cases.append(
            (
                "matrix expands and substitutes into commands and env",
                len(matrix) == 2
                and matrix[0].steps[0].command == 'echo "matrix "'
                and matrix[1].steps[0].command == 'echo "matrix memory"'
                and matrix[1].env["FEATURES"] == "memory",
                f"{[(job.variant, job.steps[0].command) for job in matrix]}",
            )
        )

        tag = translate("tag", workflow["jobs"]["tag"], include_tag_jobs=False)
        cases.append(
            (
                "a tag-gated job is refused unless asked for",
                bool(tag[0].unsupported) and "tag-gated" in str(tag[0].unsupported),
                f"{tag[0].unsupported}",
            )
        )
        tag_on = translate("tag", workflow["jobs"]["tag"], include_tag_jobs=True)
        cases.append(
            (
                "--tag runs the release job",
                tag_on[0].unsupported is None and tag_on[0].steps[0].command == "echo released",
                f"{tag_on[0].steps}",
            )
        )

        odd = translate("odd", workflow["jobs"]["odd"], include_tag_jobs=False)
        cases.append(
            (
                "an expression it cannot resolve is refused, not blanked",
                "secrets.TOKEN" in str(odd[0].unsupported),
                f"{odd[0].unsupported}",
            )
        )

        # The real workflow must translate, or the script is lying about what CI does.
        real = load_workflow(Path(WORKFLOW))
        dead = [
            name
            for name, body in (real.get("jobs") or {}).items()
            if name not in UNREPRODUCIBLE
            and any(
                job.unsupported and not benign(job.unsupported)
                for job in translate(name, body, include_tag_jobs=False)
            )
        ]
        cases.append(("this repository's workflow translates", not dead, f"untranslatable: {dead}"))
        cases.append(
            (
                "the exception list matches the workflow",
                all(name in (real.get("jobs") or {}) for name in UNREPRODUCIBLE),
                f"{sorted(UNREPRODUCIBLE)}",
            )
        )

        failed = [name for name, ok, _ in cases if not ok]
        for name, ok, detail in cases:
            print(f"  {'ok  ' if ok else 'FAIL'} {name}" + ("" if ok else f" -- {detail}"))
        if failed:
            print(f"ci_local self-test: {len(failed)} case(s) failed")
            return 1
        print(f"ci_local self-test: {len(cases)} case(s) passed")
        return 0
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main(argv: list[str]) -> int:
    """Parse the arguments and do the work."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--list", action="store_true", help="describe every job and exit")
    parser.add_argument("--check", action="store_true", help="fail if a job cannot be reproduced locally")
    parser.add_argument("--self-test", action="store_true", help="test the translator on fixtures")
    parser.add_argument("--job", action="append", default=[], metavar="NAME", help="run one job (repeatable)")
    parser.add_argument("--all", action="store_true", help="run every reproducible job")
    parser.add_argument("--keep-going", action="store_true", help="continue after a failing job")
    parser.add_argument("--tag", action="store_true", help="include jobs gated on a release tag")
    parser.add_argument("--dry-run", action="store_true", help="print the commands without running them")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    workflow = load_workflow(WORKFLOW)
    if args.check:
        return check(workflow)

    if not (args.list or args.all or args.job):
        parser.print_help()
        return 2

    jobs = jobs_of(workflow, include_tag_jobs=args.tag, wanted=args.job)
    if args.list:
        for job in jobs:
            print(summarise(job))
        return 0

    if args.dry_run:
        for job in jobs:
            print(summarise(job))
            if job.unsupported:
                continue
            for step in job.steps:
                if step.command:
                    print(colour(DIM, f"  $ {step.command}"))
        return 0

    results = []
    for job in jobs:
        label, ok = (job.name + (f" [{job.variant}]" if job.variant else ""), True)
        if job.unsupported:
            print(colour(YELLOW, f"SKIP {label}: {job.unsupported}"))
            results.append((label, None))
            continue
        label, ok = run_job(job, Path.cwd())
        results.append((label, ok))
        if not ok and not args.keep_going:
            break

    failed = [label for label, ok in results if ok is False]
    skipped = [label for label, ok in results if ok is None]
    print()
    print(f"{len(results) - len(failed) - len(skipped)} passed, {len(failed)} failed, {len(skipped)} skipped")
    if failed:
        print(colour(RED, "failed: " + ", ".join(failed)))
    if skipped:
        print(colour(YELLOW, "skipped: " + ", ".join(skipped)))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
