# Releasing

The process is boring on purpose. Every step is a command, every gate is a script, and
nothing depends on someone remembering a detail at 6 p.m. on a Friday.

## Versioning

Semantic versioning, with the crate's own definition of "public API" written down:

- **Major** — a change that breaks a downstream build, a save file, or a replay. Breaking a
  replay counts even when the types compile: behaviour is API.
- **Minor** — new capability: a leaf, a port, a preset, a scheduler knob.
- **Patch** — a bug fix that leaves every number in `docs/dev/tuning/performance.md` within its
  tolerance and every digest test unchanged.

`0.x` is not licence to break things cheaply: the digest tests and the save-format corpus
exist so that behaviour changes are *noticed*, and `cargo semver-checks` gates the type-level
part of the same promise.

## Pre-release checklist

Run from a clean checkout of the commit you intend to tag.

```sh
# 1. Everything the CI does, locally, in one go.
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test   --locked --release --all-features
cargo test   --locked --lib                       # default features
cargo check  --locked --no-default-features       # the server build
cargo check  --locked --target wasm32-unknown-unknown

# 2. The scripts that guard decisions.
python3 scripts/group_cap.py --self-test
python3 scripts/scaling_regression.py --self-test
python3 scripts/cpu_topology.py --self-test
python3 scripts/scaling_report.py --check
python3 scripts/cell_size_gate.py

# 3. The docs that quote numbers.
cargo doc --locked --no-deps --all-features        # no warnings
grep -rn "TODO\|FIXME\|XXX" src/ docs/ README.md    # an empty result is the goal

# 4. The package itself.
cargo package --locked                             # must be warning-free
```

Then the two judgement calls a script cannot make:

- **Read the changelog as a stranger.** Does `## [0.1.0]` explain what they can now do, or
  does it list commits?
- **Re-measure one number.** Pick the headline frame from `docs/dev/tuning/performance.md`, run its
  bench on your machine, and confirm the shape matches. If it does not, the table is stale
  and the release stops here.

## Cutting the release

```sh
# 1. Version, in one place, then let cargo fix the lockfiles.
cargo set-version 0.1.1          # or edit Cargo.toml by hand
cargo check --locked --all-features
(cd compat && cargo check --locked)

# 2. Changelog: move Unreleased to the new version with today's date.
$EDITOR CHANGELOG.md

# 3. Commit, tag, push both.
git add -A
git commit -m "release: 0.1.1"
git tag -a v0.1.1 -m "bevy-flock 0.1.1"
git push origin main --follow-tags
```

The tag triggers the `release` CI job, which repeats the checklist plus
`cargo semver-checks check-release --baseline-rev v0.1.0` (once a baseline tag exists) and
`cargo publish --dry-run`.

## Publishing

```sh
cargo publish --locked
```

Then, in the same hour:

1. `cargo doc` on docs.rs — check the build status rather than assuming it.
2. Open the release on GitHub, paste the changelog section, and link the ADRs the release
   added.
3. If the release changed a measured number, confirm `docs/dev/tuning/performance.md` and the README
   table agree with the new bench artifacts. Stale numbers are worse than no numbers.
4. Update the `compat` harness's pinned version in `compat/Cargo.toml`, which is what proves
   the save format still reads the previous release's files.

## What a yank means

A yank is for a release that is *wrong*, not for one that is merely slow: a broken build, a
save-format corruption, a panic in a documented path. It hides the version from new
resolutions and keeps existing lockfiles working. The fix is a patch release, never an
edited tag — a tag that moves is a tag nobody can trust.

## Support window

- The latest minor version gets fixes.
- The previous minor gets critical fixes (security, corruption, panics) for six months.
- A Bevy minor bump is a `bevy-flock` minor bump, and the migration notes go in the release
  body, not in a wiki.

## The files a release touches

| File | Why |
| --- | --- |
| `Cargo.toml` | version and (rarely) `rust-version` |
| `Cargo.lock`, `compat/Cargo.lock` | both committed, both `--locked` |
| `CHANGELOG.md` | the section a stranger reads |
| `docs/dev/tuning/performance.md` | only if a number moved, and then with a dated re-measure |
| `docs/dev/meta/release-checklist.md` | tick the sign-off, and record the host you measured on |
| `compat/Cargo.toml` | the previous release's pinned version |
