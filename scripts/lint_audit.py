#!/usr/bin/env python3
"""Fail the build if a clippy override appears outside the two sanctioned kernels.

The crate's lint policy lives in Cargo.toml: `pedantic`, `nursery`, `cargo`, `perf` and `all` are
warnings, CI passes `-D warnings`, and a named list of lints is denied outright — `unwrap_used`,
`expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment`, `suspicious` and the rustc lint
`raw_borrows_via_references`. A file-level `#[allow]` or `#[expect]` for any of those turns the
policy into a suggestion, so this script exists to keep the count at zero.

The sanctioned list below is **empty**, on purpose and as a result: `kernel-bounds-checks` converted
the last two file-level expectations (the 44 index expressions in `application::packed` and the 18 in
the wide kernel of `application::crowd`) to `get`/destructuring, so the strict spine now covers the
kernels too. The list stays because it is the mechanism: adding an override means adding it here,
with a reason and a `plan.toml` task that removes it, which is a conversation rather than a diff.

Run with `--self-test` to check the detector against fixtures rather than the tree.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: The only overrides the crate is allowed to carry, as (relative path, lint) -> reason.
#:
#: Empty is the intended state. An entry here is a decision that a denied lint is wrong for one
#: file, which needs a reason and a `plan.toml` task that removes it; see `docs/lint-policy.md`.
SANCTIONED: dict[tuple[str, str], str] = {}

#: Attributes worth finding. `cfg_attr` wraps `expect`/`allow`, so the inner lint name is what
#: matters and the search is textual rather than syntactic.
ATTRIBUTE = re.compile(r"#!?\[(?:cfg_attr\([^]]*?)?(allow|expect)\(([^]]*?)\)\]", re.S)
LINT = re.compile(r"clippy::([a-z_0-9]+)")
LEVEL = re.compile(r'level\s*=\s*"([a-z]+)"')

SEARCH = ("src", "tests", "benches", "examples", "compat", "fuzz")


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


#: What `Cargo.toml` has to say, so the policy itself cannot drift while the exceptions stay clean.
#: `(table, lint, level)`; a missing entry or a weaker level is a failure.
REQUIRED = (
    ("lints.rust", "unsafe_code", "deny"),
    ("lints.rust", "missing_docs", "deny"),
    ("lints.clippy", "suspicious", "deny"),
    ("lints.clippy", "unwrap_used", "deny"),
    ("lints.clippy", "expect_used", "deny"),
    ("lints.clippy", "panic", "deny"),
    ("lints.clippy", "indexing_slicing", "deny"),
    ("lints.clippy", "cast_ptr_alignment", "deny"),
    ("lints.clippy", "all", "warn"),
    ("lints.clippy", "pedantic", "warn"),
    ("lints.clippy", "nursery", "warn"),
    ("lints.clippy", "cargo", "warn"),
    ("lints.clippy", "perf", "warn"),
)


def manifest_levels(text: str) -> dict[tuple[str, str], str]:
    """Read `[lints.<table>]` into {(table, lint): level}, ignoring comments."""
    levels: dict[tuple[str, str], str] = {}
    table = ""
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("["):
            table = line.strip("[]")
            continue
        if not table.startswith("lints."):
            continue
        if "=" not in line:
            continue
        lint, level = line.split("=", 1)
        # Either `name = "deny"` or the table form `name = { level = "deny", priority = -1 }`.
        value = level.strip()
        if value.startswith("{"):
            found = LEVEL.search(value)
            value = found.group(1) if found else ""
        levels[(table, lint.strip())] = value.strip('"')
    return levels


def check_manifest(root: Path) -> int:
    """Fail if the manifest no longer denies what the policy says it denies."""
    path = root / "Cargo.toml"
    if not path.is_file():
        print(f"lint audit: no Cargo.toml at {path}")
        return 1
    levels = manifest_levels(path.read_text(encoding="utf-8"))
    problems = []
    for table, lint, expected in REQUIRED:
        found = levels.get((table, lint))
        if found != expected:
            problems.append(f"  [{table}] {lint}: expected {expected}, found {found or 'nothing'}")
    if problems:
        print("the lint policy in Cargo.toml is not the policy this audit enforces:")
        print("\n".join(problems))
        print()
        print("Restore the entry, or change REQUIRED here and docs/lint-policy.md together.")
        return 1
    return 0


def walk(root: Path) -> list[Path]:
    """Every Rust file the policy covers."""
    files: list[Path] = []
    for top in SEARCH:
        for path in sorted((root / top).rglob("*.rs")):
            files.append(path)
    return files


def findings(root: Path) -> tuple[list[tuple[str, int, str | None, str]], set[tuple[str, str]]]:
    """Split every override in the tree into unsanctioned ones and sanctioned ones seen."""
    unsanctioned = []
    seen = set()
    for path in walk(root):
        relative = path.relative_to(root).as_posix()
        for line, kind, lint in scan(path):
            key = (relative, lint)
            if key in SANCTIONED:
                seen.add(key)
                continue
            unsanctioned.append((relative, line, kind, lint))
    return unsanctioned, seen


def report_sanctioned() -> None:
    """Print the exceptions with their reasons, so a failure shows the whole picture."""
    if not SANCTIONED:
        return
    print("sanctioned overrides (not a failure):")
    for (relative, lint), reason in sorted(SANCTIONED.items()):
        print(f"  {relative}: clippy::{lint} -- {reason}")
    print()


def check(root: Path) -> int:
    """Check the tree; return a process exit code."""
    if check_manifest(root) != 0:
        return 1
    unsanctioned, seen_sanctioned = findings(root)
    if unsanctioned:
        print("clippy overrides outside the sanctioned list:")
        for relative, line, kind, lint in unsanctioned:
            print(f"  {relative}:{line}: {kind}(clippy::{lint})")
        print()
        print("Fix the code, or add the site to SANCTIONED in this script with a reason and a")
        print("plan.toml task that removes it.")
        print()
        report_sanctioned()
        return 1
    stale = set(SANCTIONED) - seen_sanctioned
    if stale:
        print("sanctioned overrides that are no longer in the tree:")
        for relative, lint in sorted(stale):
            print(f"  {relative}: clippy::{lint} -- was: {SANCTIONED[(relative, lint)]}")
        print()
        print("Delete them from SANCTIONED: an exception nobody needs is an exception nobody reads.")
        return 1
    print(
        f"lint audit: {len(walk(root))} file(s) scanned, {len(REQUIRED)} manifest entries verified, "
        f"{len(seen_sanctioned)} sanctioned override(s), none unsanctioned"
    )
    return 0


def write_manifest(root: Path, override: str | None = None) -> None:
    """Write a manifest that satisfies REQUIRED, optionally with one entry replaced."""
    lines = ["[package]", 'name = "fixture"', 'version = "0.1.0"', ""]
    for table in ("lints.rust", "lints.clippy"):
        lines.append(f"[{table}]")
        for section, lint, level in REQUIRED:
            if section != table:
                continue
            if override is not None and lint == override.split("=")[0].strip():
                lines.append(override)
            else:
                lines.append(f'{lint} = "{level}"')
        lines.append("")
    (root / "Cargo.toml").write_text("\n".join(lines), encoding="utf-8")


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
            unsanctioned, _ = findings(root)
            found = len(unsanctioned)
            if found != expected:
                print(f"self-test FAILED: {label}: expected {expected}, found {found}")
                failures += 1
            else:
                print(f"self-test ok: {label}")

    # The sanction mechanism itself: a listed override passes, the same override unlisted fails,
    # and a listed override with nothing behind it (a stale entry) fails.
    mechanism = [
        ("a sanctioned override passes", True, True, 0),
        ("the same override unlisted fails", True, False, 1),
        ("a stale sanction fails", False, True, 1),
    ]
    for label, present, sanctioned, expected in mechanism:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_manifest(root)
            if present:
                path = root / "src" / "probe.rs"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(
                    "#![expect(clippy::indexing_slicing, reason = \"fixture\")]\n", encoding="utf-8"
                )
            key = ("src/probe.rs", "indexing_slicing")
            if sanctioned:
                SANCTIONED[key] = "self-test only"
            try:
                code = check(root)
            finally:
                SANCTIONED.pop(key, None)
            if code != expected:
                print(f"self-test FAILED: {label}: expected exit {expected}, got {code}")
                failures += 1
            else:
                print(f"self-test ok: {label}")

    # And the policy itself: a manifest that downgrades a denied lint has to fail even though the
    # tree carries no overrides at all.
    for label, override, expected in [
        ("a downgraded deny fails", 'panic = "warn"', 1),
        ("a missing deny fails", 'unwrap_used = "allow"', 1),
        ("an intact manifest passes", 'panic = "deny"', 0),
    ]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_manifest(root, override)
            code = check(root)
            if code != expected:
                print(f"self-test FAILED: {label}: expected exit {expected}, got {code}")
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
