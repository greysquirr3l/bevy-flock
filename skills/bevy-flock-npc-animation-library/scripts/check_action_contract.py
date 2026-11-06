#!/usr/bin/env python3
"""Check that the three copies of the NPC action list agree, without Blender.

The clip set exists in three places, and each has to be there for a reason:

- `retarget_npc_actions.py` names the source clip to bake for each output stem. It is executable.
- `validate_npc_action_library.py` names what a correct library contains. It is the gate.
- `SKILL.md` documents the index order a game reads `GltfAssetLabel::Animation(n)` with. It is the
  contract a person follows.

Three copies of one list is how a list drifts. This script reads all three — Python through `ast`, so
nothing is imported and `bpy` is never needed — and fails on any disagreement, including an index
order that is not alphabetical, since that order is what the indices mean.

    python3 skills/bevy-flock-npc-animation-library/scripts/check_action_contract.py
    python3 skills/bevy-flock-npc-animation-library/scripts/check_action_contract.py --self-test
"""

from __future__ import annotations

import argparse
import ast
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SKILL = HERE.parent / "SKILL.md"
RETARGET = HERE / "retarget_npc_actions.py"
VALIDATOR = HERE / "validate_npc_action_library.py"

DOC_ROW = re.compile(
    r"^\|\s*(?P<index>\d+)\s*\|\s*`(?P<stem>[a-z_]+)`\s*\|\s*`(?P<library>[a-z0-9_]+)`\s*"
    r"`(?P<clip>[A-Za-z0-9_]+)`\s*\|\s*$"
)


def unwrap(value: ast.expr) -> object:
    """Read a literal, looking through the one-argument constructors a set constant is written with."""
    if isinstance(value, ast.Call) and len(value.args) == 1 and not value.keywords:
        if getattr(value.func, "id", None) in {"frozenset", "set", "tuple", "sorted"}:
            return unwrap(value.args[0])
    return ast.literal_eval(value)


def literal(path: Path, name: str) -> object:
    """Return the value of a module-level constant, read without executing the module."""
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == name:
                    return unwrap(node.value)
    raise SystemExit(f"contract: {path.name} has no top-level `{name}`")


def parse_retarget(path: Path) -> dict[str, tuple[str, str]]:
    """Output stem -> (source library, source clip), from the retarget table."""
    table = literal(path, "SELECTED_ACTIONS")
    mapping: dict[str, tuple[str, str]] = {}
    for row in table:  # type: ignore[union-attr]
        stem, library, clip = row
        if stem in mapping:
            raise SystemExit(f"contract: `{stem}` appears twice in SELECTED_ACTIONS")
        mapping[stem] = (library, clip)
    return mapping


def parse_validator(path: Path) -> set[str]:
    """The clip names a valid library must contain."""
    return set(literal(path, "EXPECTED_ACTIONS"))  # type: ignore[arg-type]


def parse_doc(path: Path) -> list[tuple[int, str, str, str]]:
    """Index, stem, source library, source clip from the contract table."""
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        match = DOC_ROW.match(line.strip())
        if match:
            rows.append(
                (
                    int(match.group("index")),
                    match.group("stem"),
                    match.group("library"),
                    match.group("clip"),
                )
            )
    return rows


def compare(
    baked: dict[str, tuple[str, str]],
    expected: set[str],
    documented: list[tuple[int, str, str, str]],
) -> list[str]:
    """Every way the three copies can disagree, as printable problems."""
    problems: list[str] = []
    for stem in sorted(set(baked) - expected):
        problems.append(f"the retarget script bakes `{stem}`, which the validator does not expect")
    for stem in sorted(expected - set(baked)):
        problems.append(f"the validator expects `{stem}`, which nothing bakes")
    documented_stems = [row[1] for row in documented]
    for stem in sorted(set(baked) - set(documented_stems)):
        problems.append(f"`{stem}` is baked but undocumented")
    for stem in sorted(set(documented_stems) - set(baked)):
        problems.append(f"`{stem}` is documented but nothing bakes it")
    if len(documented_stems) != len(set(documented_stems)):
        problems.append("the documented table names a clip twice")
    order = [row[0] for row in documented]
    if order != list(range(len(order))):
        problems.append(f"the documented indices are not 0..{len(order) - 1}: {order}")
    if documented_stems != sorted(documented_stems):
        problems.append(
            "the documented order is not alphabetical, but alphabetical order is the index contract: "
            f"{documented_stems} against {sorted(documented_stems)}"
        )
    for _, stem, library, clip in documented:
        source = baked.get(stem)
        if source is not None and source != (library, clip):
            problems.append(
                f"`{stem}` bakes {source[0]} `{source[1]}` but is documented as {library} `{clip}`"
            )
    return problems


def check() -> int:
    """Compare the real files; return a process exit code."""
    problems = compare(parse_retarget(RETARGET), parse_validator(VALIDATOR), parse_doc(SKILL))
    if problems:
        print("the NPC action contract disagrees with itself:")
        for problem in problems:
            print(f"  - {problem}")
        print()
        print("Fix the retarget table, the validator's set, and the SKILL.md table together.")
        return 1
    stems = sorted(parse_validator(VALIDATOR))
    print(f"action contract: {len(stems)} clips agree across three files, index order alphabetical")
    return 0


def self_test() -> int:
    """Check that `compare` reports each kind of disagreement, using fixtures rather than the tree."""
    baked = {"idle": ("ual1", "Idle_Loop"), "brace": ("ual2", "Sword_Block")}
    expected = {"idle", "brace"}
    documented = [(0, "brace", "ual2", "Sword_Block"), (1, "idle", "ual1", "Idle_Loop")]
    failures = 0

    def case(label: str, actual: list[str], wanted: int) -> None:
        nonlocal failures
        if len(actual) != wanted:
            print(f"self-test FAILED: {label}: wanted {wanted} problem(s), got {actual!r}")
            failures += 1
        else:
            print(f"self-test ok: {label}")

    case("three agreeing files report nothing", compare(baked, expected, documented), 0)
    case(
        "an unbaked expectation is caught",
        compare({"brace": ("ual2", "Sword_Block")}, expected, [(0, "brace", "ual2", "Sword_Block")]),
        1,
    )
    case(
        "an undocumented clip is caught",
        compare(baked, expected, [(0, "brace", "ual2", "Sword_Block")]),
        1,
    )
    case(
        "a source mismatch is caught",
        compare(
            baked,
            expected,
            [(0, "brace", "ual2", "Sword_Block"), (1, "idle", "ual2", "Idle_Loop")],
        ),
        1,
    )
    case(
        "a non-alphabetical order is caught",
        compare(
            {"idle": ("ual1", "Idle_Loop"), "brace": ("ual2", "Sword_Block")},
            expected,
            [(0, "idle", "ual1", "Idle_Loop"), (1, "brace", "ual2", "Sword_Block")],
        ),
        1,
    )
    case(
        "a broken index sequence is caught",
        compare(baked, expected, [(0, "brace", "ual2", "Sword_Block"), (2, "idle", "ual1", "Idle_Loop")]),
        1,
    )
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    """Entry point."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--self-test", action="store_true", help="check the comparison, not the tree")
    args = parser.parse_args(argv)
    return self_test() if args.self_test else check()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
