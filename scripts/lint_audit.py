#!/usr/bin/env python3
"""Fail the build if a lint override escapes the policy, or the policy is weakened.

The crate's lint policy lives in `Cargo.toml`: `pedantic`, `nursery`, `cargo`, `perf` and `all` are
warnings, CI passes `-D warnings`, a named list of lints is denied outright — `unwrap_used`,
`expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment`, `suspicious`, plus the rustc lints
`unsafe_code` and `missing_docs` — and a named list is allowed for reasons a machine cannot check.

Two things can break that policy, so this script checks both:

- **An override in a file.** Any `#[allow]`/`#[expect]` of any lint, clippy's or rustc's, outside
  `SANCTIONED` fails. Today that is eight entries: four are `unsafe_code`, all a counting allocator
  in a test or an example — `GlobalAlloc` is unsafe by definition, and there is no way to assert
  "this frame allocates nothing" from outside the crate without one — and four are `too_many_lines`
  on a file whose shape is the point of it. A crate-level `#[expect]` is judged by rule instead:
  `expected_is_allowed` states the four cases the policy permits, and one of them is a source
  attribute that restates a decision the package's own manifest already made.
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
        "crates/bevy-flock-anim/tests/no_alloc.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to observe an allocation; confined to one test binary",
    (
        "tests/band_continuity.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to assert gate 7",
    (
        "tests/crowd_no_alloc.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to assert zero allocations per frame",
    (
        "examples/spawn_cost.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to price the allocations a dispatch mechanism makes",
    (
        "tests/tree_no_alloc.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to assert the tree allocates nothing",
    (
        "tests/spatial_batch_no_alloc.rs",
        "unsafe_code",
    ): "a counting allocator is the only way to tell a zero-allocation batch from a bounded one",
    (
        "demo/src/city.rs",
        "too_many_lines",
    ): "the layout is one pass of the same generator over districts, blocks, shelters and pylons; \
each section is the city's own narrative and splitting it would pass a dozen mutable accumulators \
between helpers to keep a line count down",
    (
        "demo/src/plan.rs",
        "too_many_lines",
    ): "the plan is drawn in painting order -- roads, ruins, settlements, props, crowd, legend -- and \
each layer is three to eight lines; the alternative is a dozen mutable buffers in a signature",
    (
        "demo/benches/render_pipeline.rs",
        "too_many_lines",
    ): "a Criterion body that opens a device, uploads the scene and times a pass: the setup is the \
measurement's context and cannot be hoisted without hiding what is being timed",
    (
        "demo-gpu/src/lib.rs",
        "too_many_lines",
    ): "a wgpu pipeline is a description that only compiles in one order: bind groups, then the \
layout, then the pipeline that consumes them, each borrowing the last, so splitting it moves the \
borrow rather than the line count",
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
    # Restriction lints that no group turns on: `panic` covers `panic!` and leaves `todo!` and
    # `unimplemented!` allowed, which is how a half-written function ships.
    ("lints.clippy", "todo", "deny"),
    ("lints.clippy", "unimplemented", "deny"),
    ("lints.clippy", "all", "warn"),
    ("lints.clippy", "pedantic", "warn"),
    ("lints.clippy", "nursery", "warn"),
    ("lints.clippy", "cargo", "warn"),
    ("lints.clippy", "perf", "warn"),
    # The lints below were allowed with a paragraph of justification each. They are denied now, and
    # the code satisfies them; `num` and the Bevy system signatures carry the only `#[expect]`s,
    # under the rules `expected_is_allowed` states.
    ("lints.clippy", "needless_pass_by_value", "deny"),
    ("lints.clippy", "cast_precision_loss", "deny"),
    ("lints.clippy", "cast_possible_truncation", "deny"),
    ("lints.clippy", "cast_sign_loss", "deny"),
    ("lints.clippy", "cast_possible_wrap", "deny"),
    ("lints.clippy", "float_cmp", "deny"),
    ("lints.clippy", "missing_const_for_fn", "deny"),
    ("lints.clippy", "significant_drop_tightening", "deny"),
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
    "suboptimal_flops": "the kernels must not fuse: a fused op rounds once where the scalar road rounds twice",
}

#: The lints `src/domain/num.rs` is allowed to `#[expect]`: it exists so that a numeric cast happens
#: once, with its rounding written down, instead of at three hundred call sites.
CAST_LINTS = frozenset(
    {
        "cast_precision_loss",
        "cast_possible_truncation",
        "cast_sign_loss",
        "cast_possible_wrap",
    }
)
#: The conversion module: the one file permitted to carry a cast expectation. It moved into the core crate during the
#: phase-6 extraction, so both locations are accepted: the rule is about the file's role (the crate's single numeric
#: boundary), not about which crate's src/ tree it happens to sit under. Both are listed rather than one being rewritten
#: so the audit keeps passing on a tree that still has the facade-local copy.
NUM_MODULES = frozenset({"src/domain/num.rs", "crates/bevy-flock-core/src/domain/num.rs"})
#: The crate roots that carry a source-level expectation, and the manifest that governs each one.
#: A crate-level attribute applies to the crate it is written in and no other, so the root and the
#: manifest have to agree: `src/lib.rs` is the library's root and `Cargo.toml` is the library's
#: manifest.
CRATE_ROOTS = {
    "src/lib.rs": "Cargo.toml",
    "crates/bevy-flock-anim/src/lib.rs": "crates/bevy-flock-anim/Cargo.toml",
    "demo/src/lib.rs": "demo/Cargo.toml",
    "examples/build_passes.rs": "Cargo.toml",
    "examples/scaling_sweep.rs": "Cargo.toml",
    "playground/src/lib.rs": "playground/Cargo.toml",
    "tests/spatial_parity.rs": "Cargo.toml",
}
#: A Bevy system parameter, by value, as the framework's `SystemParam` contract requires.
BEVY_PARAM = re.compile(
    r"\b(?:Res|ResMut|Query|Commands|Local|EventReader|EventWriter|Single|ParamSet)<"
)
#: How far past an `#[expect]` to look for the signature it is attached to.
SIGNATURE_WINDOW = 60

#: Attributes worth finding. `cfg_attr` wraps `expect`/`allow`, so the inner lint name is what
#: matters and the search is textual rather than syntactic. The groups keep the `#!`, which is what
#: separates a crate-level attribute from one attached to an item, and the `cfg_attr` wrapper, which
#: makes the attribute conditional and so not a plain crate-level decision.
ATTRIBUTE = re.compile(
    r"(#!?)\[(?:(cfg_attr\([^]]*?\)?))?(allow|expect)\(([^]]*?)\)\]", re.S
)
#: `level = "deny"` inside the manifest's table form.
LEVEL = re.compile(r'level\s*=\s*"([a-z]+)"')
#: `reason = "…"`, stripped before an attribute body is split on commas.
NAME = re.compile(r"[a-z_][a-z0-9_]*")
# `re.S` because a reason is often wrapped across lines with a trailing `\\`, and a pattern that
# stops at the newline leaves the reason's own words looking like lint names.
REASON = re.compile(r'reason\s*=\s*"(?:[^"\\]|\\.)*"', re.S)

SEARCH = (
    "src",
    "crates",
    "tests",
    "benches",
    "examples",
    "compat",
    "fuzz",
    "demo",
    "demo-gpu",
    "playground",
)

#: Every manifest that carries the policy, relative to the repository root. A crate that is not in
#: this list has no table to weaken; one that is, cannot weaken it quietly.
MANIFESTS = (
    "Cargo.toml",
    "crates/bevy-flock-anim/Cargo.toml",
    "demo/Cargo.toml",
    "demo-gpu/Cargo.toml",
    "playground/Cargo.toml",
)

#: Lints a manifest may leave at `allow` because the framework, not the code, forces the shape.
#: Bevy hands a system its parameters by value and the reference forms are not `SystemParam`s, so
#: `needless_pass_by_value` is unsatisfiable in a crate that writes systems; the exception is
#: recorded against that one manifest and nowhere else. Every entry here needs the reason in the
#: manifest itself, where a reader of the table will find it.
TOLERATED_BY_MANIFEST = {
    "playground/Cargo.toml": {"needless_pass_by_value"},
}

#: How strong a level is. A crate may be stricter than the policy — the GPU demo denies the groups
#: instead of warning about them — but never weaker.
STRENGTH = {"allow": 0, "warn": 1, "deny": 2, "forbid": 3}


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


def scan(path: Path) -> list[tuple[int, str, str, bool, bool]]:
    """Return (line, kind, lint, crate_level, conditional) for every override in a file.

    `crate_level` is true for `#![...]` and false for `#[...]`. The distinction is what makes a
    crate-root expectation different from one attached to a single item, so the policy judges them
    by different rules. `conditional` is true when a `cfg_attr` wraps the attribute, which means the
    attribute is only present under some configuration and so cannot be a crate-wide decision.
    """
    findings = []
    text = strip_comments(path.read_text(encoding="utf-8"))
    for match in ATTRIBUTE.finditer(text):
        kind = match.group(3)
        line = text.count("\n", 0, match.start()) + 1
        crate_level = match.group(1) == "#!"
        conditional = match.group(2) is not None
        for lint in lint_names(match.group(4)):
            findings.append((line, kind, lint, crate_level, conditional))
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
    """Fail if any manifest no longer says what the policy says it says.

    Every crate that carries the table is checked, because a weakened table in the demo crate is
    exactly as invisible as one in the library. A crate may be *stricter* than the policy — the GPU
    demo denies the groups rather than warning about them, since it has no `-D warnings` step of its
    own — so the comparison is against a strength order rather than for equality.
    """
    problems = []
    covered: set[str] = set()
    # A crate added to `crates/` with a lint table of its own is exactly the crate that would slip
    # through a hand-written list: the list would still pass, over one manifest fewer. Discovery
    # closes that hole without forcing a table onto the tool crates that carry none.
    for pattern in ("*/Cargo.toml", "crates/*/Cargo.toml"):
        for path in sorted(root.glob(pattern)):
            relative = path.relative_to(root).as_posix()
            if relative in MANIFESTS:
                continue
            if "[lints" in path.read_text(encoding="utf-8"):
                problems.append(
                    f"  {relative}: carries a lint table but is not in MANIFESTS, so its policy is "
                    f"never checked"
                )
    for relative in MANIFESTS:
        path = root / relative
        if not path.is_file():
            # Silently skipping is how a rename or a delete removes a crate from the policy without
            # anyone noticing: the audit would report success over two manifests instead of three.
            problems.append(f"  {relative}: the manifest that carries the policy is missing")
            continue
        levels = manifest_levels(path.read_text(encoding="utf-8"))
        tolerated = set(TOLERATED) | TOLERATED_BY_MANIFEST.get(relative, set())
        for table, lint, expected in REQUIRED:
            if lint in TOLERATED_BY_MANIFEST.get(relative, set()):
                continue
            found = levels.get((table, lint))
            if STRENGTH.get(found or "", -1) < STRENGTH[expected]:
                problems.append(
                    f"  {relative} [{table}] {lint}: expected at least {expected}, "
                    f"found {found or 'nothing'}"
                )
        allowed = {
            lint
            for (table, lint), level in levels.items()
            if table.startswith("lints.") and level == "allow"
        }
        for lint in sorted(allowed - tolerated):
            problems.append(f"  {relative}: allowed but not in TOLERATED: {lint}")
        covered.update(allowed)
    # A crate may deny what another crate tolerates, so the tolerated set is checked once against
    # every manifest together: a lint that nothing allows any more is a stale entry.
    for lint in sorted(set(TOLERATED) - covered):
        problems.append(f"  no manifest allows the tolerated lint any more: {lint}")
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
            # `demo/`, `demo-gpu/` and `compat/` are packages, so each has its own `target/`: the
            # build scripts of dependencies write `.rs` files there, and a generated file that
            # happens to carry an `allow` is not a policy violation this repository can fix.
            if "target" in path.relative_to(root).parts:
                continue
            files.append(path)
    return files


def manifest_allows(root: Path, relative: str, lint: str) -> bool:
    """Whether the manifest that governs a file allows a lint crate-wide.

    The manifest is the authority: a crate-level expectation is only ever a *restatement* of a
    decision the manifest already made, so the audit asks the manifest whether it made it.
    """
    manifest = root / relative
    if not manifest.is_file():
        return False
    levels = manifest_levels(manifest.read_text(encoding="utf-8"))
    for (table, name), level in levels.items():
        if table.startswith("lints.") and name == lint and level == "allow":
            return True
    return False


def expected_is_allowed(
    path: Path,
    root: Path,
    relative: str,
    line: int,
    lint: str,
    crate_level: bool,
    conditional: bool,
) -> bool:
    """Whether an `#[expect]` at this site is one of the four the policy permits.

    An `#[expect]` is not a suppression: it fails the build when the lint stops firing, so it cannot
    rot into an `allow`. It is still only correct where the lint is *right about the code and wrong
    about the rule*, which is true in exactly four places here:

    * `num` owns every numeric cast, and each of its helpers says what it does with the bits.
    * A Bevy system is handed its parameters by value; `&Res<T>` is not a `SystemParam`, so the
      signature cannot be changed to satisfy the lint.
    * Criterion writes its report when the benchmark group drops, so the group's scope *is* the
      measurement; shortening its life would measure less, not more.
    * A crate root restates a decision its own manifest already made, so that the statement holds
      against the lint flags an editor appends to the command line after the manifest's.

    Everything else is code to fix, which is why the reason is required and the rules are narrow.
    """
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    if not 0 < line <= len(lines):
        return False
    attribute = "\n".join(lines[line - 1 : line + 8])
    if not REASON.search(attribute):
        return False
    if relative in NUM_MODULES:
        return lint in CAST_LINTS
    if lint == "needless_pass_by_value":
        window = "\n".join(lines[line - 1 : line + SIGNATURE_WINDOW])
        return bool(BEVY_PARAM.search(window))
    if lint == "significant_drop_tightening":
        # Criterion writes its report when the group drops, so the group's scope is the measurement.
        return relative.startswith("benches/") and "Criterion" in attribute
    if crate_level and not conditional:
        # A crate-level expectation at a crate root, naming a lint the root's own manifest allows
        # crate-wide. The manifest decides; the source says so where it outranks the command line.
        manifest = CRATE_ROOTS.get(relative)
        if manifest is not None and manifest_allows(root, manifest, lint):
            return True
    return False


def findings(root: Path) -> tuple[list[tuple[str, int, str | None, str]], set[tuple[str, str]]]:
    """Split every override in the tree into unsanctioned ones and sanctioned ones seen."""
    unsanctioned = []
    seen = set()
    for path in walk(root):
        relative = path.relative_to(root).as_posix()
        for line, kind, lint, crate_level, conditional in scan(path):
            key = (relative, lint)
            if key in SANCTIONED:
                seen.add(key)
                continue
            if kind == "expect" and expected_is_allowed(
                path, root, relative, line, lint, crate_level, conditional
            ):
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
        "override(s) and documented expectation(s), none unsanctioned"
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
    body = "\n".join(lines)
    for relative in MANIFESTS:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")


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
            "src/domain/num.rs",
            '#[expect(\n    clippy::cast_precision_loss,\n    reason = "a coordinate conversion"\n)]\n'
            "fn f(v: u32) -> f32 { v as f32 }\n",
            0,
            "a documented cast expectation in the conversion module",
        ),
        (
            "src/domain/num.rs",
            "#[expect(clippy::cast_precision_loss)]\nfn f() {}\n",
            1,
            "a cast expectation with no reason",
        ),
        (
            "src/plain.rs",
            '#[expect(\n    clippy::cast_precision_loss,\n    reason = "elsewhere"\n)]\n'
            "fn f(v: u32) -> f32 { v as f32 }\n",
            1,
            "a cast expectation outside the conversion module",
        ),
        (
            "src/sys.rs",
            '#[expect(\n    clippy::needless_pass_by_value,\n    reason = "a system"\n)]\n'
            "pub fn s(config: Res<BoidsConfig>) { let _ = config; }\n",
            0,
            "a documented expectation on a Bevy system",
        ),
        (
            "benches/x.rs",
            '#[expect(\n    clippy::significant_drop_tightening,\n'
            '    reason = "Criterion writes the report when the group drops"\n)]\n'
            "fn b() {}\n",
            0,
            "a documented Criterion guard expectation in a bench",
        ),
        (
            "src/plain.rs",
            '#[expect(\n    clippy::significant_drop_tightening,\n'
            '    reason = "Criterion writes the report when the group drops"\n)]\n'
            "fn b() {}\n",
            1,
            "a Criterion guard expectation outside the benches",
        ),
        (
            "src/plain.rs",
            '#[expect(\n    clippy::needless_pass_by_value,\n    reason = "a helper"\n)]\n'
            "pub fn h(value: String) { let _ = value; }\n",
            1,
            "a pass-by-value expectation on something that is not a system",
        ),
        (
            "src/other.rs",
            "#![cfg_attr(\n    feature = \"simd\",\n    expect(clippy::indexing_slicing, reason = \"x\")\n)]\nfn f() {}\n",
            1,
            "a cfg_attr wrapping an expect",
        ),
        (
            "demo/target/debug/build/serde/out/private.rs",
            "#![allow(clippy::unwrap_used)]\nfn f() {}\n",
            0,
            "a build artefact is not the repository's code",
        ),
        (
            "src/sys.rs",
            '#[expect(\n    clippy::needless_pass_by_value,\n'
            '    reason = "a system hands its \\\nparameters over by value"\n)]\n'
            "pub fn s(config: Res<BoidsConfig>) { let _ = config; }\n",
            0,
            "a reason wrapped across lines does not become a lint name",
        ),
        (
            "src/lib.rs",
            '#![expect(\n    clippy::suboptimal_flops,\n    reason = "the kernels must not fuse"\n)]\n'
            "fn f() {}\n",
            0,
            "a crate-level expectation restating the manifest's crate-wide allow",
        ),
        (
            "crates/bevy-flock-anim/src/lib.rs",
            '#![expect(\n    clippy::suboptimal_flops,\n'
            '    reason = "animation blends must preserve unfused deterministic rounding"\n)]\n'
            "fn f() {}\n",
            0,
            "the animation crate root restates its manifest's crate-wide allow",
        ),
        (
            "examples/build_passes.rs",
            '#![expect(\n    clippy::suboptimal_flops,\n'
            '    reason = "benchmark fixtures must preserve unfused deterministic rounding"\n)]\n'
            "fn f() {}\n",
            0,
            "an example crate root restates the library manifest's crate-wide allow",
        ),
        (
            "playground/src/lib.rs",
            '#![expect(\n    clippy::suboptimal_flops,\n'
            '    reason = "playground transforms must preserve unfused deterministic rounding"\n)]\n'
            "fn f() {}\n",
            0,
            "the playground crate root restates its manifest's crate-wide allow",
        ),
        (
            "tests/spatial_parity.rs",
            '#![expect(\n    clippy::suboptimal_flops,\n'
            '    reason = "parity fixtures must preserve unfused deterministic rounding"\n)]\n'
            "fn f() {}\n",
            0,
            "an integration-test crate root restates the library manifest's crate-wide allow",
        ),
        (
            "src/lib.rs",
            '#![expect(\n    clippy::unwrap_used,\n    reason = "the manifest denies this"\n)]\n'
            "fn f() {}\n",
            1,
            "a crate-level expectation of a lint the manifest does not allow",
        ),
        (
            "src/module.rs",
            '#![expect(\n    clippy::suboptimal_flops,\n    reason = "not a crate root"\n)]\n'
            "fn f() {}\n",
            1,
            "a crate-level expectation that is not at a crate root",
        ),
        (
            "src/lib.rs",
            '#![cfg_attr(\n    feature = "simd",\n    expect(clippy::suboptimal_flops, reason = "x")\n)]\n'
            "fn f() {}\n",
            1,
            "a conditional crate-level expectation",
        ),
    ]
    failures = 0
    for relative, body, expected, label in cases:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # The manifests are written because the crate-root rule consults them: an expectation
            # that restates a manifest decision is only recognised when the manifest is there.
            write_manifest(root)
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
    # The one exception recorded against a manifest: honoured where it is written down, refused
    # anywhere else, so the reason has to name a crate that cannot satisfy the lint.
    for label, relative, expected in [
        ("the recorded manifest exception is honoured", "playground/Cargo.toml", 0),
        ("the exception does not leak to another manifest", "demo/Cargo.toml", 1),
    ]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_manifest(root)
            path = root / relative
            path.write_text(
                path.read_text(encoding="utf-8").replace(
                    'needless_pass_by_value = "deny"', 'needless_pass_by_value = "allow"'
                ),
                encoding="utf-8",
            )
            saved = dict(SANCTIONED)
            SANCTIONED.clear()
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
    # A manifest that carries the policy and is not there is a failure. Without this, renaming or
    # deleting a crate's manifest removes it from the audit and the run still reports success.
    for label, removed, expected in [
        ("every manifest present passes", None, 0),
        ("a missing manifest fails", "demo/Cargo.toml", 1),
    ]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_manifest(root)
            if removed is not None:
                (root / removed).unlink()
            # The real tree's sanctions have no files here, and a stale one is its own failure.
            saved = dict(SANCTIONED)
            SANCTIONED.clear()
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
        ("a tolerated allow that vanished fails", None, None, "must_use_candidate", 1),
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

    # A second manifest is judged on its own: a crate may be stricter than the library, but it may
    # not allow something the library does not.
    for label, extra, expected in [
        ("a crate stricter than the policy passes", None, 0),
        ("a second manifest with an undocumented allow fails", 'dead_code = "allow"', 1),
    ]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_manifest(root)
            text = (root / "Cargo.toml").read_text(encoding="utf-8")
            text = text.replace('suboptimal_flops = "allow"', 'suboptimal_flops = "deny"')
            if extra is not None:
                text = text.replace("[lints.clippy]", f"[lints.clippy]\n{extra}", 1)
            nested = root / "demo-gpu" / "Cargo.toml"
            nested.parent.mkdir(parents=True, exist_ok=True)
            nested.write_text(text, encoding="utf-8")
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
