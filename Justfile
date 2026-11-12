# bevy-flock: the gates, one recipe each.
#
# `just` with no arguments lists the recipes. Every recipe runs something the repository already
# defines: a cargo command from the project rules, or a script under `scripts/`. None of them is a
# second copy of a gate, so a recipe cannot drift from the thing it stands for. The CI jobs are read
# out of `.github/workflows/ci.yml` by `scripts/ci_local.py`, and the publishable manifests are read
# out of the manifests by `scripts/publish_check.py`.
#
# The gate scripts need Python 3.11 or newer (`tomllib`, `zip(strict=True)`). The default prefers a
# `.venv` in the tree, then the newest interpreter on PATH; `just python-venv` creates one, and
# `PYTHON=/path/to/python3` chooses your own.

set shell := ["bash", "-euo", "pipefail", "-c"]

# The interpreter the gate scripts run under.
python := env_var_or_default("PYTHON", `test -x .venv/bin/python && echo .venv/bin/python || (command -v python3.15 || command -v python3.14 || command -v python3.13 || command -v python3.12 || command -v python3.11 || command -v python3 || echo python3)`)

# List the recipes.
default:
    @just --list --unsorted

# Check the interpreter the gate scripts need, and say what is missing when it is too old.
python-check:
    @{{python}} -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)' || { \
        echo "the gate scripts need Python 3.11 or newer: tomllib is 3.11, zip(strict=True) is 3.10."; \
        echo "set PYTHON=/path/to/python3 to choose another interpreter."; \
        exit 1; \
    }

# Create an interpreter for the gate scripts, with the one dependency the local CI runner needs.
python-venv:
    {{python}} -m venv .venv
    .venv/bin/python -m pip install --upgrade pip
    .venv/bin/python -m pip install pyyaml
    @echo "use it with: PYTHON=.venv/bin/python just ci"

# Check the local CI runner's one dependency, which is not in the standard library.
python-deps:
    @{{python}} -c 'import yaml' 2>/dev/null || { \
        echo "scripts/ci_local.py needs PyYAML: {{python}} -m pip install pyyaml"; \
        echo "or run 'just python-venv' and use PYTHON=.venv/bin/python."; \
        exit 1; \
    }

# Build every target with every feature.
build:
    cargo build --workspace --all-targets --all-features

# Run the tests.
test:
    cargo test --workspace --all-features

# Clippy, with the warnings denied.
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Check the formatting.
fmt:
    cargo fmt --all -- --check

# Format the code.
fmt-write:
    cargo fmt --all

# Build the server shape: the simulation core with no feature at all.
core-only:
    cargo check --workspace --no-default-features

# The licence and advisory audit.
deny:
    cargo deny check

# The build the project rules require before a commit.
preflight: build test clippy deny

# Every CI job, run locally and read out of the workflow file.
ci: python-check python-deps
    {{python}} scripts/ci_local.py --all --keep-going

# The gate scripts' own self-tests, which check the gates against fixtures rather than the tree.
scripts: python-check
    {{python}} scripts/group_cap.py --self-test
    {{python}} scripts/scaling_regression.py --self-test
    {{python}} scripts/cpu_topology.py --self-test
    {{python}} scripts/audit_summary.py --self-test
    {{python}} scripts/cell_size_gate.py --self-test
    {{python}} scripts/lint_audit.py --self-test
    {{python}} scripts/publish_check.py --self-test
    {{python}} scripts/cross_bench.py --self-test
    {{python}} scripts/lint_drift.py --self-test
    {{python}} scripts/check_docs_links.py --self-test
    {{python}} scripts/ci_local.py --self-test

# The lints that exist in a newer toolchain than the pinned one, and the drift catalogue.
lint-audit: python-check
    {{python}} scripts/lint_audit.py
    {{python}} scripts/lint_drift.py --toolchain stable

# The docs tree's links. Markdown does not complain about a broken one, so they are checked.
docs-links: python-check
    {{python}} scripts/check_docs_links.py

# The prose that states a measured number against the code that measures it.
scaling-report: python-check
    {{python}} scripts/scaling_report.py --check

# The cell size the grid derives from its budget.
cell-size: python-check
    {{python}} scripts/cell_size_gate.py

# Build the documentation site. The site's dependencies have to be installed once: `npm ci` in `site/`.
site:
    cd site && npm run build

# Serve the documentation site.
site-serve:
    cd site && npm run dev

# Check that the manifests are publishable, and print the order they have to go up in.
publish-check: python-check
    {{python}} scripts/publish_check.py

# Package every crate. `cargo package --workspace` resolves the workspace's internal path
# dependencies against the workspace rather than against crates.io, so all four package
# offline -- including the first release, when none of them is published yet.
package:
    cargo package --locked --workspace

# Set every crate's version, and the version requirements that point at them.
set-version version:
    cargo set-version --workspace {{version}}

# The pre-release checklist from RELEASING.md, in one run.
release-check: preflight scripts lint-audit docs-links publish-check package

# The type-level API against the last published release. Needs `cargo semver-checks`, which is not
# installed by default: `cargo install cargo-semver-checks`.
semver baseline="v0.1.0-rc.1":
    cargo semver-checks check-release --baseline-rev {{baseline}} --feature-group all-features

# Run the benchmarks that have a committed baseline.
bench:
    cargo bench --workspace --all-features
