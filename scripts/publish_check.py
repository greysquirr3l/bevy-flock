#!/usr/bin/env python3
"""Check the workspace is publishable, in the order it has to be published.

`cargo publish --dry-run` cannot answer this question for a workspace of path dependencies *when it
is asked about one crate*. The first crate verifies; every crate above it fails with

    no matching package named `bevy-flock-core` found

because the packaged manifest no longer contains a path, only a version requirement, and that
version is not on crates.io yet. That is the ordering constraint working, not a defect.

`cargo publish --dry-run --workspace` does not have that problem: it resolves the workspace's
internal path dependencies against the workspace, so all four crates verify at once, on the first
release as well as every one after it. Cargo 1.90 stabilised that.

So this script checks the things that must be true before the first upload, all of which are
readable from the manifests:

  1. every publishable crate carries the metadata crates.io requires, and its README exists
     (a `readme = "README.md"` that points at nothing fails the upload after the build)
  2. every path dependency also carries a `version` whose requirement accepts the dependency's
     own version, because that is all the published facade will see
  3. no publishable crate depends on a `publish = false` crate, which is the trap a workspace
     falls into when a member is temporarily unshipped
  4. the resulting dependency graph is acyclic, and its topological order is printed, because
     that order is the publish order

`--version` prints the one version the publishable crates agree on, which is the version a release
tag names. The crates have to agree: a tag of `v1.0.0` over a crate still saying `1.0.1` publishes
a version the tag does not name, and crates.io accepts that silently.

Run with `--self-test` to check the checks against fixtures rather than the real workspace.
"""

from __future__ import annotations

import sys
import tempfile
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

REQUIRED_FIELDS = ("description", "license", "repository", "documentation", "readme", "rust-version")
MAX_KEYWORDS = 5
MAX_KEYWORD_LEN = 20
MAX_CATEGORIES = 5

# crates.io accepts a closed list of category slugs and answers an upload that names another
# one with a 400, after the upload has started and the crates before it are already published.
# The list is `https://crates.io/api/v1/categories`, refreshed by hand: it has not moved in
# years, and a stale entry here fails the gate loudly, where a manifest that names a slug
# crates.io does not have fails the release silently.
#
# `animation` was the one that cost a release: it is not a crates.io category, and the upload
# stopped with `bevy-flock-core` and `bevy-flock-ecs` already on crates.io. `game-engines` is
# where Bevy's own crates sit, so it is where this one belongs too.
CRATES_IO_CATEGORIES = frozenset(
    {
        "api-bindings",
        "accessibility",
        "aerospace",
        "algorithms",
        "artificial-intelligence",
        "asynchronous",
        "authentication",
        "automotive",
        "caching",
        "command-line-interface",
        "command-line-utilities",
        "compilers",
        "compression",
        "computer-vision",
        "concurrency",
        "config",
        "cryptography",
        "data-structures",
        "database",
        "database-implementations",
        "date-and-time",
        "development-tools",
        "email",
        "embedded",
        "emulators",
        "encoding",
        "external-ffi-bindings",
        "filesystem",
        "finance",
        "game-development",
        "game-engines",
        "games",
        "graphics",
        "gui",
        "hardware-support",
        "internationalization",
        "localization",
        "mathematics",
        "memory-management",
        "multimedia",
        "network-programming",
        "no-std",
        "os",
        "parser-implementations",
        "parsing",
        "rendering",
        "rust-patterns",
        "science",
        "security",
        "simulation",
        "template-engine",
        "text-editors",
        "text-processing",
        "value-formatting",
        "virtualization",
        "visualization",
        "wasm",
        "web-programming",
    }
)


class Problem(Exception):
    """One manifest is not publishable, with the reason a human needs."""


def load(manifest: Path) -> dict:
    with manifest.open("rb") as handle:
        return tomllib.load(handle)


def workspace_members(root: Path) -> list[Path]:
    """Every member manifest of the workspace rooted at `root`, root package included."""
    root_manifest = root / "Cargo.toml"
    data = load(root_manifest)
    manifests = []
    if "package" in data:
        manifests.append(root_manifest)
    workspace = data.get("workspace", {})
    for pattern in workspace.get("members", []):
        for member in sorted(root.glob(pattern)):
            manifest = member / "Cargo.toml" if member.is_dir() else member
            if manifest.exists():
                manifests.append(manifest)
    return manifests


def is_publishable(package: dict) -> bool:
    """`publish = false` opts out; a list restricts to those registries; absent means crates.io."""
    publish = package.get("publish")
    return publish is not False


def check_metadata(manifest: Path, package: dict) -> None:
    name = package.get("name", manifest.parent.name)
    for field in REQUIRED_FIELDS:
        if not package.get(field):
            raise Problem(f"{name}: [package] has no {field}")
    readme = manifest.parent / package["readme"]
    if not readme.is_file():
        raise Problem(f"{name}: readme = {package['readme']!r} does not exist at {readme}")

    keywords = package.get("keywords") or []
    if not keywords:
        raise Problem(f"{name}: no keywords, so it is unfindable on crates.io")
    if len(keywords) > MAX_KEYWORDS:
        raise Problem(f"{name}: {len(keywords)} keywords, crates.io allows {MAX_KEYWORDS}")
    for keyword in keywords:
        if len(keyword) > MAX_KEYWORD_LEN:
            raise Problem(f"{name}: keyword {keyword!r} is longer than {MAX_KEYWORD_LEN} characters")
        if not keyword.replace("-", "").replace("_", "").isalnum() or not keyword.isascii():
            raise Problem(f"{name}: keyword {keyword!r} is not ASCII alphanumeric, hyphen or underscore")

    categories = package.get("categories") or []
    if not categories:
        raise Problem(f"{name}: no categories")
    if len(categories) > MAX_CATEGORIES:
        raise Problem(f"{name}: {len(categories)} categories, crates.io allows {MAX_CATEGORIES}")
    for category in categories:
        if category not in CRATES_IO_CATEGORIES:
            raise Problem(
                f"{name}: category {category!r} is not one crates.io accepts; "
                f"the upload would be answered with a 400"
            )

    # `[package.metadata.docs.rs]` is three TOML keys, not one: tomllib reads it as metadata.docs.rs
    metadata = package.get("metadata") or {}
    docs_rs = (metadata.get("docs") or {}).get("rs") or metadata.get("docs.rs") or {}
    if docs_rs.get("all-features") is not True:
        raise Problem(f"{name}: [package.metadata.docs.rs] does not set all-features = true")


def caret_accepts(wanted: list[int], got: list[int]) -> bool:
    """Whether `got` falls inside the range the caret requirement `^wanted` names.

    Cargo's rule is that the leftmost non-zero component is fixed and the components to its right
    may change: `^1.2.3` is `>=1.2.3, <2.0.0`, `^0.2.3` is `>=0.2.3, <0.3.0`, and `^0.0.3` is
    `>=0.0.3, <0.0.4`. The `0.x` and `1.x` eras therefore need different rules, and a checker that
    knew only the `0.x` one rejected every `1.0.0` manifest it was meant to clear.
    """
    width = max(len(wanted), len(got), 3)
    wanted = wanted + [0] * (width - len(wanted))
    got = got + [0] * (width - len(got))
    if got < wanted:
        return False
    if wanted[0] != 0:
        upper = [wanted[0] + 1, 0, 0]
    elif wanted[1] != 0:
        upper = [0, wanted[1] + 1, 0]
    else:
        upper = [0, 0, wanted[2] + 1]
    return got < upper


def requirement_accepts(requirement: str, version: str) -> bool:
    """Whether a crates.io version requirement accepts `version`.

    Only the forms this workspace uses are understood, and an unrecognised form is an error rather
    than a guess: a checker that quietly accepts `*` would pass a manifest that pins nothing.
    """
    for comparator in requirement.split(","):
        comparator = comparator.strip()
        if not comparator:
            continue
        caret = comparator.startswith("^")
        exact = comparator.startswith("=")
        if caret or exact:
            comparator = comparator[1:]
        elif comparator[0] in "<>~*":
            raise Problem(f"requirement {requirement!r}: only caret and exact forms are checked")
        wanted = [int(part) for part in comparator.split(".")]
        got = [int(part) for part in version.split(".")]
        if exact:
            if wanted != got:
                return False
        elif not caret_accepts(wanted, got):
            return False
    return True


def dependencies(document: dict) -> list[dict]:
    """The document's dependencies as a list of tables, each carrying its own name.

    Two traps live here. `[dependencies]` is a top-level table, not a `[package]` field, so reading it
    off the package table finds nothing and the check silently passes. And it is a table keyed by name
    rather than a list, so iterating it yields strings. Renaming is spelled `package = "..."`, which is
    the name that matters on the registry.
    """
    table = document.get("dependencies") or {}
    entries = []
    for key, value in table.items():
        # `rhai = "1"` is the common spelling, and it is a string, not a table.
        if isinstance(value, str):
            entries.append({"name": key, "version": value})
        elif isinstance(value, dict):
            entries.append({"name": value.get("package", key), **value})
        else:
            raise Problem(f"dependency {key!r} is neither a version string nor a table")
    return entries


def check_path_dependencies(
    manifest: Path, document: dict, versions: dict[str, str], publishable: dict[str, bool]
) -> None:
    name = document["package"].get("name", manifest.parent.name)
    for dep in dependencies(document):
        dep_name = dep["name"]
        if dep.get("path") is None:
            continue
        if dep_name not in versions:
            raise Problem(f"{name}: path dependency {dep_name!r} is not a workspace member")
        if not dep.get("version"):
            raise Problem(f"{name}: path dependency {dep_name!r} has no version, so the published manifest cannot resolve it")
        if not requirement_accepts(dep["version"], versions[dep_name]):
            raise Problem(
                f"{name}: requires {dep_name} {dep['version']!r}, workspace has {versions[dep_name]}"
            )
        if not publishable.get(dep_name, True):
            raise Problem(f"{name}: depends on {dep_name!r}, which is publish = false and would not exist on the registry")


def publish_order(edges: dict[str, set[str]]) -> list[str]:
    """Kahn's algorithm, alphabetical at each step so the printed order is stable."""
    order = []
    remaining = {name: set(deps) for name, deps in edges.items()}
    while remaining:
        ready = sorted(name for name, deps in remaining.items() if not deps)
        if not ready:
            cycle = ", ".join(sorted(remaining))
            raise Problem(f"dependency cycle among workspace crates: {cycle}")
        for name in ready:
            order.append(name)
            del remaining[name]
        for deps in remaining.values():
            deps.difference_update(ready)
    return order


def agreed_version(packages: dict[str, tuple[Path, dict]]) -> str:
    """The one version every publishable crate carries.

    This workspace has no `[workspace.package] version` to inherit from, so the four
    manifests each carry their own. A release tags one version, so the crates have to
    agree on it: a tag of `v1.0.0` over a crate still saying `1.0.1` publishes a
    version the tag does not name, and crates.io accepts that silently.
    """
    publishable = {
        name: document["package"]["version"]
        for name, (_, document) in packages.items()
        if is_publishable(document["package"])
    }
    distinct = sorted(set(publishable.values()))
    if len(distinct) > 1:
        listed = ", ".join(f"{name} {version}" for name, version in sorted(publishable.items()))
        raise Problem(f"the publishable crates do not agree on one version: {listed}")
    if not distinct:
        raise Problem("no publishable crate in the workspace")
    return distinct[0]


def workspace_packages(root: Path) -> dict[str, tuple[Path, dict]]:
    """Every member manifest of the workspace, keyed by package name."""
    packages = {}
    for manifest in workspace_members(root):
        data = load(manifest)
        packages[data["package"]["name"]] = (manifest, data)
    return packages


def run(root: Path) -> list[str]:
    packages = workspace_packages(root)
    versions = {name: document["package"]["version"] for name, (_, document) in packages.items()}
    publishable = {name: is_publishable(document["package"]) for name, (_, document) in packages.items()}

    edges: dict[str, set[str]] = {}
    for name, (manifest, document) in packages.items():
        if not publishable[name]:
            continue
        check_metadata(manifest, document["package"])
        check_path_dependencies(manifest, document, versions, publishable)
        edges[name] = {dep["name"] for dep in dependencies(document) if dep.get("path") is not None}
    return publish_order(edges)


def write_fixture(root: Path, crates: dict[str, dict]) -> None:
    """Write a workspace whose members are described by `crates`, one manifest each."""
    for name, spec in crates.items():
        member = root / name
        member.mkdir()
        readme = spec.get("readme", True)
        if readme:
            (member / "README.md").write_text("# member\n")
        lines = [
            "[package]",
            f'name = "{name}"',
            f'version = "{spec.get("version", "0.1.0")}"',
            'description = "d"',
            'license = "MIT"',
            'repository = "r"',
            'documentation = "u"',
            'readme = "README.md"' if readme else 'readme = "ABSENT.md"',
            'rust-version = "1.97"',
        ]
        keywords = spec.get("keywords", ["k"])
        if keywords:
            quoted = ", ".join(f'"{keyword}"' for keyword in keywords)
            lines.append(f"keywords = [{quoted}]")
        categories = spec.get("categories", ["game-development"])
        quoted = ", ".join(f'"{category}"' for category in categories)
        lines.append(f"categories = [{quoted}]")
        if spec.get("publish") is False:
            lines.append("publish = false")
        if spec.get("docs_rs", True):
            lines += ["", "[package.metadata.docs.rs]", "all-features = true"]
        deps = spec.get("deps") or {}
        if deps:
            lines += ["", "[dependencies]"]
            lines += [f"{dep} = {table}" for dep, table in deps.items()]
        (member / "Cargo.toml").write_text("\n".join(lines) + "\n")
    members = ", ".join(f'"{name}"' for name in crates)
    (root / "Cargo.toml").write_text(f'[workspace]\nmembers = [{members}]\nresolver = "3"\n')


def self_test() -> None:
    """The checks have to fail on a broken manifest, or they are decoration.

    Every case here is a mistake this repository has actually made or could make: a README field
    pointing at nothing, `publish = false` left over from a temporary state, a path dependency
    without a version, and the two shapes that only turn up once there are four crates.
    """
    dependency = '{ version = "0.1.0", path = "leaf" }'
    cases = {
        "missing readme": {"leaf": {"readme": False}},
        "no keywords": {"leaf": {"keywords": []}},
        "too many keywords": {"leaf": {"keywords": ["a", "b", "c", "d", "e", "f"]}},
        "no docs.rs all-features": {"leaf": {"docs_rs": False}},
        "category crates.io does not have": {
            "leaf": {"categories": ["game-development", "animation"]},
        },
        "too many categories": {
            "leaf": {"categories": ["a", "b", "c", "d", "e", "f"]},
        },
        "path dep without version": {"leaf": {}, "root": {"deps": {"leaf": '{ path = "leaf" }'}}},
        "publish = false dependency": {
            "leaf": {"publish": False},
            "root": {"deps": {"leaf": dependency}},
        },
        "cycle": {
            "leaf": {"deps": {"root": '{ version = "0.1.0", path = "../root" }'}},
            "root": {"deps": {"leaf": dependency}},
        },
    }
    for label, crates in cases.items():
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_fixture(root, crates)
            try:
                run(root)
            except Problem:
                continue
            raise SystemExit(f"self-test failed: {label!r} was accepted")

    # The control: a well-formed pair is accepted, and the order is dependencies first. A checker that
    # rejects everything is as useless as one that accepts everything.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        write_fixture(root, {"leaf": {}, "root": {"deps": {"leaf": dependency}}})
        order = run(root)
        if order != ["leaf", "root"]:
            raise SystemExit(f"self-test failed: order {order} is not dependencies-first")

    # The version rules, which changed shape when the crates left `0.x`: a caret requirement fixes
    # the leftmost non-zero component, so `^1.0.0` and `^0.1.0` are not the same rule.
    version_cases = [
        ("^1.0.0 accepts 1.0.0", "^1.0.0", "1.0.0", True),
        ("^1.0.0 accepts 1.9.3", "^1.0.0", "1.9.3", True),
        ("^1.0.0 rejects 2.0.0", "^1.0.0", "2.0.0", False),
        ("^1.0.0 rejects 0.9.0", "^1.0.0", "0.9.0", False),
        ("^0.1.0 accepts 0.1.7", "^0.1.0", "0.1.7", True),
        ("^0.1.0 rejects 0.2.0", "^0.1.0", "0.2.0", False),
        ("=1.0.0 accepts 1.0.0", "=1.0.0", "1.0.0", True),
        ("=1.0.0 rejects 1.0.1", "=1.0.0", "1.0.1", False),
    ]
    for label, requirement, version, expected in version_cases:
        if requirement_accepts(requirement, version) != expected:
            raise SystemExit(f"self-test failed: {label}")

    # The version agreement, which is what the release tag is built from. Two crates
    # carrying different versions is the mistake that publishes `v1.0.0` over a crate
    # that says `1.0.1`, and it has to be caught here rather than at the registry.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        write_fixture(root, {"leaf": {"version": "1.2.3"}, "root": {"version": "1.2.3"}})
        packages = workspace_packages(root)
        if agreed_version(packages) != "1.2.3":
            raise SystemExit("self-test failed: agreeing versions did not come back")
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        write_fixture(root, {"leaf": {"version": "1.2.3"}, "root": {"version": "1.2.4"}})
        packages = workspace_packages(root)
        try:
            agreed_version(packages)
        except Problem:
            pass
        else:
            raise SystemExit("self-test failed: disagreeing versions were accepted")
    # A crate that is not published does not vote on the version: it is not going to
    # crates.io, so its version is not the one the tag names.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        write_fixture(
            root,
            {"leaf": {"version": "1.2.3"}, "root": {"version": "9.9.9", "publish": False}},
        )
        packages = workspace_packages(root)
        if agreed_version(packages) != "1.2.3":
            raise SystemExit("self-test failed: an unpublished crate changed the version")

    print("self-test ok: every broken fixture is rejected, and a valid pair comes back in order")


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        self_test()
        return 0
    try:
        if "--version" in argv:
            # The release workflow derives the tag from this, so it prints the version
            # alone: the tag is `v` plus that string.
            print(agreed_version(workspace_packages(REPO)))
            return 0
        order = run(REPO)
    except Problem as problem:
        print(f"not publishable: {problem}", file=sys.stderr)
        return 1
    print("publish order (dependencies first):")
    for position, name in enumerate(order, start=1):
        print(f"  {position}. {name}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
