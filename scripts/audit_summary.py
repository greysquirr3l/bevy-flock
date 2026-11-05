#!/usr/bin/env python3
"""Summarise a `cargo audit` JSON report, so the pre-push hook can name what it found.

`cargo audit` finds things and exits non-zero. That is enough for CI, where the exit code is the
whole verdict, but not for a hook: "the advisory scan failed" tells a developer nothing about which
crate to look at. This script reads the report and says what is in it, by category, with the
advisory ids and the crate names.

The categories are read separately because cargo audit files them separately. A plain vulnerability
lands under `vulnerabilities`; `unsound`, `unmaintained` and `yanked` land under `warnings`. A
reader that only looks at `vulnerabilities` calls a lockfile carrying a use-after-free advisory
clean.

    cargo audit --json --file Cargo.lock | python3 scripts/audit_summary.py

Exit codes:
    0  nothing to report — no advisory, no yanked crate
    1  something was found; the summary is on stdout
    2  the report could not be read; the report is not evidence of anything

Exit code 2 matters. `cargo audit` exits 1 both for a finding and for a failure to fetch the
advisory database, and the failure prints nothing on stdout. A scan that did not run is not a scan
that passed, so an unreadable report is never reported as a clean bill of health.
"""

from __future__ import annotations

import argparse
import json
import sys

# The advisory categories this crate's supply-chain policy denies, and the key each one is filed
# under in cargo audit's report.
ADVISORY_CATEGORIES = ("unsound", "unmaintained")


def _advisory_id(entry: dict) -> str:
    """The advisory id of one entry, or a marker when the entry carries none."""
    advisory = entry.get("advisory") or {}
    return advisory.get("id", "?")


def _package_label(entry: dict) -> str:
    """The `name version` label of one entry, for the crates that have no advisory id."""
    package = entry.get("package") or {}
    name = package.get("name", "?")
    version = package.get("version", "?")
    return f"{name} {version}"


def collect(report: dict) -> tuple[list[str], list[str]]:
    """Return the advisory ids and the yanked crate labels found in a report."""
    advisories: list[str] = []

    found = report.get("vulnerabilities") or {}
    for entry in found.get("list") or []:
        advisories.append(_advisory_id(entry))

    warnings = report.get("warnings") or {}
    for category in ADVISORY_CATEGORIES:
        for entry in warnings.get(category) or []:
            advisories.append(_advisory_id(entry))

    yanked = [_package_label(entry) for entry in warnings.get("yanked") or []]

    return advisories, yanked


def summarise(report: dict) -> tuple[int, str]:
    """Return the exit code and the human-readable summary of a parsed report."""
    advisories, yanked = collect(report)

    lines: list[str] = []
    if advisories:
        lines.append(f"{len(advisories)} advisory(ies) not waived: {' '.join(advisories)}")
    else:
        lines.append("no advisories")

    if yanked:
        lines.append(f"{len(yanked)} yanked crate(s): {' '.join(yanked)}")
    else:
        lines.append("no yanked crates")

    return (1 if advisories or yanked else 0), "\n".join(lines)


def _sample_reports() -> list[tuple[str, dict]]:
    """Reports that stand for the shapes cargo audit emits."""
    return [
        (
            "clean",
            {
                "vulnerabilities": {"found": False, "count": 0, "list": []},
                "warnings": {"unmaintained": [], "unsound": [], "yanked": []},
            },
        ),
        (
            "plain vulnerability",
            {
                "vulnerabilities": {
                    "found": True,
                    "count": 1,
                    "list": [{"advisory": {"id": "RUSTSEC-2024-0001"}}],
                },
                "warnings": {"unmaintained": [], "unsound": [], "yanked": []},
            },
        ),
        (
            "unsound advisory",
            {
                "vulnerabilities": {"found": False, "count": 0, "list": []},
                "warnings": {"unmaintained": [], "unsound": [{"advisory": {"id": "RUSTSEC-2024-0002"}}], "yanked": []},
            },
        ),
        (
            "unmaintained advisory",
            {
                "vulnerabilities": {"found": False, "count": 0, "list": []},
                "warnings": {"unmaintained": [{"advisory": {"id": "RUSTSEC-2020-0361"}}], "unsound": [], "yanked": []},
            },
        ),
        (
            "yanked crate",
            {
                "vulnerabilities": {"found": False, "count": 0, "list": []},
                "warnings": {
                    "unmaintained": [],
                    "unsound": [],
                    "yanked": [{"package": {"name": "left-pad", "version": "1.0.0"}}],
                },
            },
        ),
    ]


def self_test() -> int:
    """Check the parsing against the report shapes cargo audit emits."""
    failures: list[str] = []

    code, text = summarise(_sample_reports()[0][1])
    if code != 0:
        failures.append("a clean report should exit 0")
    if "no advisories" not in text or "no yanked crates" not in text:
        failures.append("a clean report should say so on both lines")

    for name, report in _sample_reports()[1:]:
        code, text = summarise(report)
        if code != 1:
            failures.append(f"{name} should exit 1")
        if "no advisories" in text and name != "yanked crate":
            failures.append(f"{name} should not report a clean graph")

    # The advisory ids are what a developer searches for, so they must survive the round trip.
    _, text = summarise(_sample_reports()[2][1])
    if "RUSTSEC-2024-0002" not in text:
        failures.append("an unsound advisory id should be named")

    _, text = summarise(_sample_reports()[4][1])
    if "left-pad 1.0.0" not in text:
        failures.append("a yanked crate should be named with its version")

    for failure in failures:
        print(f"  ✗ {failure}")
    if failures:
        return 1
    print("audit_summary: self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--file", help="read the report from this file instead of stdin")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="check the parsing against the report shapes cargo audit emits",
    )
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    try:
        raw = open(args.file, encoding="utf-8").read() if args.file else sys.stdin.read()
    except OSError as error:
        print(f"could not read the advisory report: {error}")
        return 2

    try:
        report = json.loads(raw)
    except ValueError:
        print("could not read the advisory report")
        return 2

    code, text = summarise(report)
    print(text)
    return code


if __name__ == "__main__":
    sys.exit(main())
