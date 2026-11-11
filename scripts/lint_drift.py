#!/usr/bin/env python3
"""Catalogue what a newer stable's clippy says, and fail when the catalogue is stale.

The crate builds on the toolchain `rust-toolchain.toml` pins (1.97.1, Bevy 0.20's MSRV), so a lint
added or sharpened after that release never runs in the job that gates the build. This script runs
clippy under a newer toolchain, extracts every warning with its file and line, and compares that set
with the table in `docs/dev/lint/lint-drift.md`.

Both directions fail, because both are drift: a warning nobody catalogued is debt that will surface
in the pin-bump commit, and a catalogued warning that is gone is a page describing a tree that no
longer exists.

    python3 scripts/lint_drift.py             # check the tree against the page
    python3 scripts/lint_drift.py --update    # rewrite the page from a live run
    python3 scripts/lint_drift.py --log f.txt # parse a saved log instead of running clippy
    python3 scripts/lint_drift.py --self-test # check the parser against fixtures

Exit codes: 0 in sync, 1 drift, 2 the script could not tell (a failed build must never read as
"nothing to report").
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PAGE = ROOT / "docs" / "dev" / "lint" / "lint-drift.md"
START = "<!-- lint-drift:start -->"
END = "<!-- lint-drift:end -->"

#: The default toolchain to read. `stable` is what CI installs beside the pin; a developer can point
#: this at `1.100.0` to see what the next release will say before it lands.
DEFAULT_TOOLCHAIN = "stable"

#: One sentence per lint, so the page says what the diagnostic wants in a reader's terms rather than
#: repeating clippy's phrasing eleven times. A lint that appears here without an entry is a failure:
#: the catalogue is not allowed to grow a row nobody wrote a sentence for.
WHAT_IT_WANTS = {
    "assert_is_empty": "`assert_eq!(x, [])` or `assert_ne!(x, \"\")`, so a failing assert prints the value",
    "chunks_exact_to_as_chunks": "`as_chunks::<N>()`, which hands out `&[T; N]` instead of a slice to re-check",
    "manual_slice_fill": "`slice.fill(value)`, which is one call and vectorises",
    "needless_borrows_for_generic_args": "pass the closure by value; the call already takes it by value",
    "suboptimal_flops": "`f32::mul_add`, which fuses the multiply and the add into one rounding",
    "tuple_array_conversions": "`<[T; N]>::into()`, or a `let [a, b] = …` binding, instead of `(v[0], v[1])`",
}

#: `warning: <message>` starts a finding; the `-->` line gives its site; the URL gives its name.
BLOCK = re.compile(r"^warning: ", re.M)
LOCATION = re.compile(r"^\s*--> ([^\s:]+):(\d+):(\d+)", re.M)
LINT = re.compile(r"index\.html#(\w+)")
ROW = re.compile(r"\| `(?P<lint>[a-z_]+)` \| `(?P<site>[^`]+)` \|")
VERSION = re.compile(r"clippy on \*\*(?P<version>[0-9][^\s*]*)\*\*")


def toolchain_version(toolchain: str) -> str:
    """The version string of a toolchain, e.g. `1.99.1`.

    Asked of rustc rather than read from the log: `cargo clippy` prints no cargo banner, so a log
    carries no version at all, and a catalogue that silently records an empty version cannot tell
    whether it is describing the toolchain it was derived from.
    """
    result = subprocess.run(
        ["rustup", "run", toolchain, "rustc", "--version"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        print(f"lint drift: cannot read the version of toolchain `{toolchain}`:")
        print(result.stderr.strip())
        raise SystemExit(2)
    return result.stdout.split()[1]


ANSI = re.compile(r"\x1b\[[0-9;]*m")


def parse(log: str) -> set[tuple[str, str]]:
    """Return the (lint, site) pairs in a clippy log, with colour codes removed first."""
    log = ANSI.sub("", log)
    warnings: set[tuple[str, str]] = set()
    bodies = BLOCK.split(log)[1:]
    for body in bodies:
        head = body.splitlines()[0] if body.splitlines() else ""
        # The per-crate summary lines ("`bevy-flock` (lib) generated 5 warnings") are not findings.
        if "generated" in head or "warnings emitted" in head:
            continue
        site = LOCATION.search(body)
        lint = LINT.search(body)
        if site and lint:
            warnings.add((lint.group(1), f"{site.group(1)}:{site.group(2)}"))
    return warnings


def run_clippy(toolchain: str) -> str:
    """Run clippy under `toolchain` and return its output, erroring if it did not build."""
    command = [
        "cargo",
        f"+{toolchain}",
        "clippy",
        "--locked",
        "--all-targets",
        "--all-features",
    ]
    # No `--offline` here: that is the environment's decision, not the script's. Set
    # `CARGO_NET_OFFLINE=true` on a machine without network access.
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    output = result.stdout + result.stderr
    if result.returncode != 0:
        print("lint drift: clippy did not finish, so there is nothing to compare:")
        print("\n".join(output.splitlines()[-20:]))
        raise SystemExit(2)
    return output


def read_page() -> tuple[str, set[tuple[str, str]]]:
    """Return the recorded toolchain version and the (lint, site) pairs in the page's table."""
    text = PAGE.read_text(encoding="utf-8")
    if START not in text or END not in text:
        print(f"lint drift: {PAGE} has no {START} … {END} region")
        raise SystemExit(2)
    region = text.split(START, 1)[1].split(END, 1)[0]
    version = VERSION.search(region)
    recorded = {
        (match.group("lint"), match.group("site")) for match in ROW.finditer(region)
    }
    return (version.group("version") if version else ""), recorded


def render(version: str, warnings: set[tuple[str, str]]) -> str:
    """Render the page region for a run."""
    counts: dict[str, int] = {}
    for lint, _ in warnings:
        counts[lint] = counts.get(lint, 0) + 1
    lints = ", ".join(f"`{lint}` × {count}" for lint, count in sorted(counts.items()))
    noun = "warning" if len(warnings) == 1 else "warnings"
    lint_noun = "lint" if len(counts) == 1 else "lints"
    lines = [
        "",
        f"clippy on **{version}** (`cargo +{DEFAULT_TOOLCHAIN} clippy --all-targets --all-features`)",
        f"reports **{len(warnings)} {noun}** across {len(counts)} lints: {lints}.",
        "",
        "| Lint | Site | What it wants |",
        "| --- | --- | --- |",
    ]
    for lint, site in sorted(warnings):
        wants = WHAT_IT_WANTS.get(lint)
        if wants is None:
            print(f"lint drift: no sentence for `{lint}` in WHAT_IT_WANTS")
            raise SystemExit(2)
        lines.append(f"| `{lint}` | `{site}` | {wants} |")
    lines.append("")
    return "\n".join(lines)


def write_page(region: str) -> None:
    """Replace the page's region, leaving the prose around it alone."""
    text = PAGE.read_text(encoding="utf-8")
    head, rest = text.split(START, 1)
    _, tail = rest.split(END, 1)
    PAGE.write_text(f"{head}{START}{region}{END}{tail}", encoding="utf-8")


def diff(recorded: set[tuple[str, str]], live: set[tuple[str, str]]) -> str:
    """Describe the difference between the page and the tree, one site per line."""
    lines = []
    for lint, site in sorted(live - recorded):
        lines.append(f"  + {lint} at {site} — present in the tree, not in the page")
    for lint, site in sorted(recorded - live):
        lines.append(f"  - {lint} at {site} — in the page, not in the tree")
    return "\n".join(lines)


def check(log: str | None, toolchain: str) -> int:
    """Compare the tree against the page; return a process exit code."""
    output = log if log is not None else run_clippy(toolchain)
    live = parse(output)
    version = toolchain_version(toolchain)
    recorded_version, recorded = read_page()
    if recorded_version and version and recorded_version != version:
        print(
            f"lint drift: the page was written against {recorded_version} and this run used "
            f"{version}. The catalogue is about a specific clippy, so it has to be re-derived:"
        )
        print("  python3 scripts/lint_drift.py --update")
        return 1
    if not live and not recorded:
        print(f"lint drift: nothing to report on {version or toolchain} — the page is empty and so is the run")
        return 0
    if live != recorded:
        print(f"lint drift: {len(live)} warning(s) in the tree, {len(recorded)} in the page:")
        print(diff(recorded, live))
        print()
        print("Fix the site, then re-derive the page:")
        print("  python3 scripts/lint_drift.py --update")
        return 1
    print(
        f"lint drift: {len(live)} warning(s) catalogued for {version or toolchain}, "
        f"all present, none unlisted"
    )
    return 0


def self_test() -> int:
    """Check the parser and the comparison against fixtures, not against the tree."""
    failures = 0

    def case(label: str, actual: object, expected: object) -> None:
        nonlocal failures
        if actual != expected:
            print(f"self-test FAILED: {label}: expected {expected!r}, got {actual!r}")
            failures += 1
        else:
            print(f"self-test ok: {label}")

    parsed = parse(
        """warning: used `assert!` to check that a value is empty
   --> src/a.rs:12:9
    |
    = help: for further information visit https://rust-lang.github.io/rust-clippy/master/index.html#assert_is_empty
warning: `bevy-flock` (lib) generated 1 warning
"""
    )
    case("a finding keeps its lint and site", parsed, {("assert_is_empty", "src/a.rs:12")})

    summary_only = parse(
        """warning: `bevy-flock` (lib) generated 5 warnings
warning: `bevy-flock` (lib test) generated 11 warnings (5 duplicates)
"""
    )
    case("summary lines are not findings", summary_only, set())

    # The comparison has to move in both directions, or a page that is merely short reads as clean.
    case(
        "an unlisted warning is reported",
        diff({("a", "f.rs:1")}, {("a", "f.rs:1"), ("b", "g.rs:2")}).strip(),
        "+ b at g.rs:2 — present in the tree, not in the page",
    )
    case(
        "a stale row is reported",
        diff({("a", "f.rs:1"), ("b", "g.rs:2")}, {("a", "f.rs:1")}).strip(),
        "- b at g.rs:2 — in the page, not in the tree",
    )

    region = render("1.99.1", {("assert_is_empty", "src/a.rs:12")})
    case("the rendered region names the toolchain", VERSION.search(region).group("version"), "1.99.1")
    case(
        "the rendered region counts one lint",
        len(list(ROW.finditer(region))),
        1,
    )
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    """Entry point."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--update", action="store_true", help="rewrite the page from a live run")
    parser.add_argument("--log", type=Path, help="parse this clippy log instead of running clippy")
    parser.add_argument("--toolchain", default=DEFAULT_TOOLCHAIN, help="toolchain to read")
    parser.add_argument("--self-test", action="store_true", help="check the parser against fixtures")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()
    if args.update:
        output = args.log.read_text(encoding="utf-8") if args.log else run_clippy(args.toolchain)
        version = toolchain_version(args.toolchain)
        live = parse(output)
        write_page(render(version, live))
        print(f"lint drift: catalogued {len(live)} warning(s) for {version}")
        return 0
    return check(args.log.read_text(encoding="utf-8") if args.log else None, args.toolchain)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
