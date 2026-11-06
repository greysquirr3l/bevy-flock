#!/usr/bin/env python3
"""Fail the build if a clippy override appears outside the two sanctioned kernels.

The crate's lint policy lives in Cargo.toml: `pedantic`, `nursery`, `cargo`, `perf` and `all` are
warnings, CI passes `-D warnings`, and a named list of lints is denied outright — `unwrap_used`,
`expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment`, `suspicious` and the rustc lint
`raw_borrows_via_references`. A file-level `#[allow]` or `#[expect]` for any of those turns the
policy into a suggestion, so this script exists to keep the count at zero.

Two exceptions are sanctioned today, both in the steering kernels and both tracked as
`kernel-bounds-checks` in `plan.toml`. They are listed below with the reason they are allowed, and
the reason is printed if the check fails, so an override cannot be added quietly.

Run with `--self-test` to check the detector against fixtures rather than the tree.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: The only overrides the crate is allowed to carry, as (relative path, lint) pairs.
SANCTIONED = {
    ("src/application/packed.rs", "indexing_slicing"): (
        "44 index expressions, each bounded by a length check in the same function; removing them "
        "is a measured kernel change (plan.toml: kernel-bounds-checks)"
    ),
    ("src/application/crowd.rs", "indexing_slicing"): (
        "24 index expressions in the wide kernel, each bounded by a length check in the same "
        "function; removing them is a measured kernel change (kernel-bounds-checks)"
    ),
}

#: Attributes worth finding. `cfg_attr` wraps `expect`/`allow`, so the inner lint name is what
#: matters and the search is textual rather than syntactic.
ATTRIBUTE = re.compile(r"#!?\[(?:cfg_attr\([^]]*?)?(allow|expect)\(([^]]*?)\)\]", re.S)
LINT = re.compile(r"clippy::([a-z_0-9]+)")

SEARCH = ("src", "tests", "benches", "examples")


def strip_comments(text: str) -> str:
    """Blank out comments, keeping every other character and every newline in place.

    Doc comments in this crate talk *about* the overrides they forbid, so a scanner that read them
    would report its own policy as a violation. Offsets are preserved so line numbers stay true.
    """
    out = list(text)
    index = 0
    while index < len(out):
        if out[index] == "/" and index + 1 < len(out):
            if out[index + 1] == "/":
                while index < len(out) and out[index] != "\n":
                    out[index] = " "
                    index += 1
                continue
            if out[index + 1] == "*":
                while index < len(out) and not (out[index] == "*" and index + 1 < len(out) and out[index + 1] == "/"):
                    if out[index] != "\n":
                        out[index] = " "
                    index += 1
                for offset in (index, index + 1):
                    if offset < len(out):
                        out[offset] = " "
                index += 2
                continue
        index += 1
    return "".join(out)


def scan(path: Path) -> list[tuple[int, str | None, str]]:
    """Return (line, kind, lint) for every clippy override in a file."""
    findings = []
    text = strip_comments(path.read_text(encoding="utf-8"))
    for match in ATTRIBUTE.finditer(text):
        kind = match.group(1)
        for lint in LINT.findall(match.group(2)):
            line = text.count("\n", 0, match.start()) + 1
            findings.append((line, kind, lint))
    return findings


def walk(root: Path) -> list[Path]:
    """Every Rust file the policy covers."""
    files: list[Path] = []
    for top in SEARCH:
        for path in sorted((root / top).rglob("*.rs")):
            files.append(path)
    return files


def check(root: Path) -> int:
    """Check the tree; return a process exit code."""
    unsanctioned = []
    seen_sanctioned = set()
    for path in walk(root):
        relative = path.relative_to(root).as_posix()
        for line, kind, lint in scan(path):
            key = (relative, lint)
            if key in SANCTIONED:
                seen_sanctioned.add(key)
                continue
            unsanctioned.append((relative, line, kind, lint))
    if unsanctioned:
        print("clippy overrides outside the sanctioned list:")
        for relative, line, kind, lint in unsanctioned:
            print(f"  {relative}:{line}: {kind}(clippy::{lint})")
        print()
        print("Fix the code, or add the site to SANCTIONED in this script with a reason and a")
        print("plan.toml task that removes it.")
        return 1
    stale = set(SANCTIONED) - seen_sanctioned
    if stale:
        print("sanctioned overrides that are no longer in the tree:")
        for relative, lint in sorted(stale):
            print(f"  {relative}: clippy::{lint}")
        print()
        print("Delete them from SANCTIONED: an exception nobody needs is an exception nobody reads.")
        return 1
    print(f"lint audit: {len(seen_sanctioned)} sanctioned override(s), none unsanctioned")
    return 0


def self_test() -> int:
    """Check the detector against fixtures written to a temporary tree."""
    import tempfile

    cases = [
        ("src/clean.rs", "fn f() {}\n", 0, "a file with no attribute"),
        (
            "src/bad.rs",
            "#![allow(clippy::unwrap_used)]\nfn f() {}\n",
            1,
            "a file-level allow",
        ),
        (
            "tests/bad.rs",
            "#[expect(clippy::panic, reason = \"x\")]\nfn f() {}\n",
            1,
            "an item-level expect",
        ),
        (
            "src/application/packed.rs",
            "#![expect(\n    clippy::indexing_slicing,\n    reason = \"bounded\"\n)]\nfn f() {}\n",
            0,
            "the sanctioned kernel",
        ),
        (
            "src/other.rs",
            "#![cfg_attr(\n    feature = \"simd\",\n    expect(clippy::indexing_slicing, reason = \"x\")\n)]\nfn f() {}\n",
            1,
            "a cfg_attr wrapping an expect",
        ),
    ]
    failures = 0
    for relative, body, expected, label in cases:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body, encoding="utf-8")
            found = 0
            for scanned in walk(root):
                for key in [(scanned.relative_to(root).as_posix(), lint) for _, _, lint in scan(scanned)]:
                    if key not in SANCTIONED:
                        found += 1
            if found != expected:
                print(f"self-test FAILED: {label}: expected {expected}, found {found}")
                failures += 1
            else:
                print(f"self-test ok: {label}")
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    """Entry point."""
    if "--self-test" in argv:
        return self_test()
    return check(ROOT)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
