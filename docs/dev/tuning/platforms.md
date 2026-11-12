# Platform and feature audit

What this crate claims to support, and the evidence for each claim. The rule for this
document is the same as for the performance numbers: if it is not measured here, it is
not claimed.

## The short version

| Claim | Evidence |
| --- | --- |
| Any target Rust can build `bevy_ecs` for | No OS-specific code in `src/` at all: no `cfg(windows)`, no `cfg(unix)`, no `libc`, no `std::os`. |
| `wasm32-unknown-unknown` | `cargo check --target wasm32-unknown-unknown` in every feature configuration below. |
| The feature matrix is orthogonal | Nine configurations compile; the two features that gate code (`memory`, `serde`) are checked with and without each other. |
| No hidden engine dependency | `cargo tree` shows `bevy_ecs`, `bevy_app`, `bevy_math`, `bevy_transform` and `bevy_utils`, no renderer, no physics, no windowing. |

## The audit

Run on 2026-11-05, Rust 1.97.1 (the pinned MSRV) with `--locked`, from a clean
`target/`.

### Feature configurations, host target

| Configuration | Result |
| --- | --- |
| `(default)` | compiles |
| `--no-default-features` | compiles |
| `--features memory` | compiles |
| `--features serde` | compiles |
| `--features memory,serde` | compiles |
| `--features simd` | compiles |
| `--all-features` | compiles |
| `--no-default-features --features memory` | compiles |
| `--no-default-features --features serde` | compiles |

The two `--no-default-features` rows are the ones that used to fail: the memory leaves
read encounter flags and the diagnostics read a memory row, and both were reaching into
`memory`-gated modules without saying so. The audit found it, and the fix is the pattern
the whole crate uses, restate the wire values (`FLAG_ATTACKED`, `FLAG_ROBBED`) with a
test that pins them to `EncounterKind::flag`, rather than import a gated module.

### `wasm32-unknown-unknown`

| Configuration | Result |
| --- | --- |
| `--no-default-features` | compiles |
| `--no-default-features --features memory` | compiles |
| `--no-default-features --features memory,serde` | compiles |
| `--features memory,simd` | compiles |

Nothing in the simulation path needs a clock or a filesystem. Threads are optional and only
ever a speed choice: the `threads` feature (on by default) spreads a steer pass over the ECS
compute pool, and `default-features = false` builds compile and behave identically on one thread
(ADR 0044). wasm builds compile with the pool present; a browser gives it one worker unless the
game configures more, and `Crowd::set_road` pins the wide road when that is not wanted. The save
format is bytes in and bytes out, which is the only thing wasm makes awkward (no `File`), and the
crate never opened one.

### What is *not* claimed

- **No browser test.** `cargo check` proves the types and the codegen; it does not prove a
  browser runtime. A game shipping to wasm should run its own crowd, because the
  interesting failures there are timing and memory, not compilation.
- **No CI job for other operating systems yet.** The audit above is single-host. Windows
  and macOS runners are cheap and belong in CI; until they are there, the claim is "no
  platform-specific code", not "tested on Windows".
- **`simd` is not automatic.** It is a default feature, but a target without the vector
  types `wide` needs falls back to the scalar path at compile time. The parity test
  (`the_vectorised_path_agrees_with_the_scalar_reference`) is what keeps that honest.

## CI coverage today

| Job | What it proves |
| --- | --- |
| `fmt`, `clippy` | the lint profile holds at `-D warnings` |
| `test` | the suite, including doctests |
| `features` | the powerset compiles (this document's table, automated) |
| `msrv` | the pinned 1.97.1 toolchain still builds |
| `lockfile` | `Cargo.lock` is committed, current and used (`--locked`) |
| `determinism` | the same seed produces the same world |
| `docs` | rustdoc builds without warnings |
| `examples` | every example builds and runs headless |

Every one of those jobs says `--workspace`. The selection decides what is checked, and it
was wrong once: a bare `cargo test` at the root of a non-virtual workspace selects the root
package alone, so the member crates' unit tests and test targets were never built. CI was
green over 109 tests while 738 unit tests in `bevy-flock-core`, `bevy-flock-ecs` and
`bevy-flock-anim` never ran, and two clippy errors sat in `bevy-flock-core`'s test code
unseen. The manifest now sets `default-members` so a local `cargo test` matches, and the
jobs name `--workspace` so the gate does not depend on that default holding.

The gap this document exists to record: no `wasm32` job and no second OS. Both are one
workflow block each, and both are in the plan rather than in the README's claims.

## Reproducing the audit

```bash
rustup target add wasm32-unknown-unknown
for features in "" "--no-default-features" "--features memory" "--features serde" \
                "--features memory,serde" "--features simd" "--all-features"; do
  cargo check --locked $features || echo "FAILED: $features"
  cargo check --locked --target wasm32-unknown-unknown $features \
    || echo "FAILED wasm: $features"
done
```

Two loops, nine configurations, about four minutes on the reference host.
