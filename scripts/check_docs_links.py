#!/usr/bin/env python3
"""Fail on a documentation link that points at nothing.

The docs tree was reorganised once (`docs/adr` -> `docs/dev/adr`, tuning material into `docs/dev/tuning`) and eleven
relative links broke silently, because Markdown does not complain and nobody clicks every link in a tuning journal. The
same class of mistake is waiting in the next reorganisation, so it is checked rather than remembered.

    python3 scripts/check_docs_links.py
    python3 scripts/check_docs_links.py --self-test

Relative links are resolved against the file that contains them. Absolute links to this site's own routes
(`/bevy-flock/...`) are skipped: they are routes, not files, and Starlight validates those at build time.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys
import tempfile

REPO = pathlib.Path(__file__).resolve().parent.parent
TREES = ("docs", "site/src/content/docs")
TOP_LEVEL = ("README.md", "CHANGELOG.md")
CRATE_READMES = ("crates",)
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)")
SKIP_PREFIXES = ("http://", "https://", "mailto:", "#", "/bevy-flock", "tel:")
SKIP_PARTS = {"node_modules", "target", "dist", ".astro"}


def documented_files() -> list[pathlib.Path]:
    files: set[pathlib.Path] = set()
    for tree in TREES:
        files.update(path for path in (REPO / tree).rglob("*.md*") if path.is_file())
    for tree in CRATE_READMES:
        files.update(path for path in (REPO / tree).rglob("README.md") if path.is_file())
    for name in TOP_LEVEL:
        candidate = REPO / name
        if candidate.is_file():
            files.add(candidate)
    return sorted(path for path in files if not SKIP_PARTS & set(path.parts))


def broken_links(files: list[pathlib.Path]) -> list[tuple[str, str]]:
    broken = []
    for path in files:
        for target in LINK.findall(path.read_text()):
            if target.startswith(SKIP_PREFIXES) or target.startswith("{"):
                continue
            clean = target.split("#")[0].split("?")[0]
            if not clean:
                continue
            if not (path.parent / clean).resolve().exists():
                try:
                    shown = str(path.relative_to(REPO))
                except ValueError:
                    shown = str(path)
                broken.append((shown, target))
    return broken


def self_test() -> int:
    """Prove the checker sees a good link and a bad one, in a tree that is not this repository."""
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        (root / "docs").mkdir()
        (root / "docs" / "target.md").write_text("# here\n")
        (root / "docs" / "page.md").write_text(
            "[good](target.md) [good anchor](target.md#section) [route](/bevy-flock/x/) "
            "[web](https://example.invalid) [missing](nope.md) [also missing](../outside.md)\n"
        )
        broken = broken_links(sorted((root / "docs").glob("*.md")))
        reported = sorted(target for _, target in broken)
        if reported != ["../outside.md", "nope.md"]:
            print(f"self-test: expected the two missing links, got {reported}", file=sys.stderr)
            return 1
    print("check_docs_links self-test: a good link passes, a missing one fails, routes and anchors are skipped")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--self-test", action="store_true", help="check the checker against a synthetic tree")
    arguments = parser.parse_args()

    if arguments.self_test:
        return self_test()

    files = documented_files()
    broken = broken_links(files)
    if broken:
        print(f"{len(broken)} broken relative link(s) across {len(files)} file(s):", file=sys.stderr)
        for path, target in broken:
            print(f"  {path}: {target}", file=sys.stderr)
        return 1

    print(f"docs links: {len(files)} file(s) scanned, every relative link resolves")
    return 0


if __name__ == "__main__":
    sys.exit(main())
