#!/usr/bin/env python3
"""Fail the build if a lint override escapes the policy, or the policy is weakened.

The crate's lint policy lives in `Cargo.toml`: `pedantic`, `nursery`, `cargo`, `perf` and `all` are
warnings, CI passes `-D warnings`, a named list of lints is denied outright — `unwrap_used`,
`expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment`, `suspicious`, plus the rustc lints
`unsafe_code` and `missing_docs` — and a named list is allowed for reasons a machine cannot check.

Two things can break that policy, so this script checks both:

- **An override in a file.** Any `#[allow]`/`#[expect]` of any lint, clippy's or rustc's, outside
  `SANCTIONED` fails. Today that is four entries, all `unsafe_code`, all a counting allocator in a
  test or an example — `GlobalAlloc` is unsafe by definition, and there is no way to assert "this
  frame allocates nothing" from outside the crate without one.
- **A weakened manifest.** `REQUIRED` must still be denied and `TOLERATED` must still be exactly the
  allowed set, so a lint cannot be quietly downgraded in a file nobody reads.

Run with `--self-test` to check the detector against fixtures rather than the tree.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: The only overrides the crate is allowed to carry, as (relative path, lint) -> reason.
#:
#: Lint names are bare: `clippy::indexing_slicing` and `indexing_slicing` both key on
#: `indexing_slicing`, so a clippy lint and a rustc lint sit in one table. An entry here is a
#: decision that a denied lint is wrong for one file, which needs a reason and a `plan.toml` task
#: that removes it; see `docs/lint-policy.md`.
SANCTIONED: dict[tuple[str, str], str] = {
    (
        "examples/memory_probe.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to report steady-state allocation",
    (
        "tests/band_continuity.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to assert gate 7",
    (
        "tests/crowd_no_alloc.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to assert zero allocations per frame",
    (
        "tests/tree_no_alloc.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to assert the tree allocates nothing",
}

#: What `Cargo.toml` has to deny or warn, so the policy cannot drift while the exceptions stay clean.
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

#: Every lint the manifest may allow, and the reason the spine cannot be satisfied without it. The
#: audit fails if the manifest allows anything outside this list, or if a lint here is no longer
#: allowed, so the table and `docs/lint-policy.md` cannot drift apart.
TOLERATED: dict[str, str] = {
    "module_name_repetitions": "the domain vocabulary is the API; `NpcMemory` in `memory` reads better than `Memory`",
    "must_use_candidate": "a getter that is obviously pure does not need an attribute to say so",
    "missing_errors_doc": "every fallible constructor names its error type in `# Errors` prose, not per-variant",
    "missing_panics_doc": "the crate denies `panic`, so the section would be empty in every signature",
    "struct_excessive_bools": "`BoidsConfig` and friends are flags by design; splitting them hides the defaults",
    "multiple_crate_versions": "Bevy's dependency graph is not ours to flatten",
    "needless_pass_by_value": "Bevy hands systems their parameters by value; the signature is the framework's",
    "cast_precision_loss": "an f32 simulation converts at audited boundaries; `cast_lossless` is still denied, so widening uses `From`",
    "cast_possible_truncation": "counts narrow to lane values at audited boundaries, bounded by `MAX_AGENTS`",
    "cast_sign_loss": "cell offsets and lane counts are non-negative by construction",
    "cast_possible_wrap": "index arithmetic stays inside `i32` by the same bound as truncation",
    "suboptimal_flops": "fusing into `mul_add` changes rounding, and the formulas are pinned bit-exactly",
    "float_cmp": "exact comparison is the substance of the determinism tests",
    "significant_drop_tightening": "Criterion's `BenchmarkGroup` is a guard by design",
    "missing_const_for_fn": "`const` is added where it enables a static, not for its own sake",
}

#: Attributes worth finding. `cfg_attr` wraps `expect`/`allow`, so the inner lint name is what
#: matters and the search is textual rather than syntactic.
ATTRIBUTE = re.compile(r"#!?\[(?:cfg_attr\([^]]*?\)?)?(allow|expect)\(([^]]*?)\)\]", re.S)
#: `level = "deny"` inside the manifest's table form.
LEVEL = re.compile(r'level\s*=\s*"([a-z]+)"')
#: `reason = "…"`, stripped before an attribute body is split on commas.
NAME = re.compile(r"[a-z_][a-z0-9_]*")
REASON = re.compile(r'reason\s*=\s*"(?:[^"\\]|\\.)*"')

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


def lint_names(body: str) -> list[str]:
    """The lint names in an attribute body, bare, with any `reason = "…"` removed first.

    A reason can contain a comma, so it is stripped as a unit before the body is split. Names are
    returned without a `clippy::` prefix, which is what lets one table hold `unsafe_code` and
    `indexing_slicing` side by side.
    """
    names = []
    for part in REASON.sub("", body).split(","):
        name = part.strip()
        if "=" in name:
            continue
        name = name.split("::")[-1]
        # A `cfg_attr` prefix drags its closing paren into the captured body; anything that is not
        # an identifier is punctuation, not a lint.
        if not NAME.fullmatch(name):
            continue
        names.append(name)
    return names


def scan(path: Path) -> list[tuple[int, str | None, str]]:
    """Return (line, kind, lint) for every override in a file, clippy's or rustc's."""
    findings = []
    text = strip_comments(path.read_text(encoding="utf-8"))
    for match in ATTRIBUTE.finditer(text):
        kind = match.group(1)
        line = text.count("\n", 0, match.start()) + 1
        for lint in lint_names(match.group(2)):
            findings.append((line, kind, lint))
    return findings


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
    """Fail if the manifest no longer says what the policy says it says."""
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
    allowed = {
        lint
        for (table, lint), level in levels.items()
        if table.startswith("lints.") and level == "allow"
    }
    for lint in sorted(allowed - set(TOLERATED)):
        problems.append(f"  allowed but not in TOLERATED: {lint}")
    for lint in sorted(set(TOLERATED) - allowed):
        problems.append(f"  in TOLERATED but no longer allowed: {lint}")
    if problems:
        print("the lint policy in Cargo.toml is not the policy this audit enforces:")
        print("\n".join(problems))
        print()
        print("Restore the entry, or change REQUIRED/TOLERATED here and docs/lint-policy.md")
        print("together.")
        return 1
    for lint, reason in sorted(TOLERATED.items()):
        assert reason
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
        print(f"  {relative}: {lint} -- {reason}")
    print()


def check(root: Path) -> int:
    """Check the tree; return a process exit code."""
    if check_manifest(root) != 0:
        return 1
    unsanctioned, seen_sanctioned = findings(root)
    if unsanctioned:
        print("lint overrides outside the sanctioned list:")
        for relative, line, kind, lint in unsanctioned:
            print(f"  {relative}:{line}: {kind}({lint})")
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
            print(f"  {relative}: {lint} -- was: {SANCTIONED[(relative, lint)]}")
        print()
        print("Delete them from SANCTIONED: an exception nobody needs is an exception nobody reads.")
        return 1
    print(
        f"lint audit: {len(walk(root))} file(s) scanned, {len(REQUIRED)} required and "
        f"{len(TOLERATED)} tolerated manifest entries verified, {len(seen_sanctioned)} sanctioned "
        "override(s), none unsanctioned"
    )
    return 0


def write_manifest(
    root: Path,
    override: str | None = None,
    extra_allow: str | None = None,
    drop_allow: str | None = None,
) -> None:
    """Write a manifest the audit accepts, with one entry optionally replaced, added or dropped."""
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
        if table == "lints.clippy":
            for lint in TOLERATED:
                if lint != drop_allow:
                    lines.append(f'{lint} = "allow"')
            if extra_allow is not None:
                lines.append(f'{extra_allow} = "allow"')
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
            "tests/alloc.rs",
            "#![expect(\n    unsafe_code,\n    reason = \"a counting allocator\"\n)]\nfn f() {}\n",
            1,
            "a bare rustc lint in an expect",
        ),
        (
            "examples/bare.rs",
            "#![allow(unsafe_code)]\nfn f() {}\n",
            1,
            "a bare rustc lint in an allow",
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
            saved = dict(SANCTIONED)
            SANCTIONED.clear()
            if sanctioned:
                SANCTIONED[key] = "self-test only"
            try:
                code = check(root)
            finally:
                SANCTIONED.clear()
                SANCTIONED.update(saved)
            if code != expected:
                print(f"self-test FAILED: {label}: expected exit {expected}, got {code}")
                failures += 1
            else:
                print(f"self-test ok: {label}")

    # And the policy itself: a manifest that downgrades a denied lint has to fail even though the
    # tree carries no overrides at all.
    for label, override, extra, dropped, expected in [
        ("a downgraded deny fails", 'panic = "warn"', None, None, 1),
        ("an allow in place of a deny fails", 'unwrap_used = "allow"', None, None, 1),
        ("an undocumented allow fails", None, "dead_code", None, 1),
        ("a tolerated allow that vanished fails", None, None, "float_cmp", 1),
        ("an intact manifest passes", 'panic = "deny"', None, None, 0),
    ]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_manifest(root, override, extra, dropped)
            saved = dict(SANCTIONED)
            SANCTIONED.clear()
            try:
                code = check(root)
            finally:
                SANCTIONED.update(saved)
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
