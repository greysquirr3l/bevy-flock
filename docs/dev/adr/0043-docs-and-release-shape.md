# ADR 0043: Documentation and release shape

**Status:** Accepted (2026-11-05)

## Context

By the end of the engine work the crate had 42 ADRs, a 370-line README, a performance document
with provenance, and no narrative. Three specific problems:

1. **The README could not be checked.** It made eight numeric claims and pointed at
   `docs/dev/tuning/performance.md` for all of them. A sceptical reader had to trust the pointer.
2. **Nothing taught the model.** The ADRs explain *why* a decision was taken, in the order the
   decisions were taken. Nobody arrives at a crate wanting the history of the spatial index;
   they want a crowd, then an explanation of the cost, in that order.
3. **The release path was folklore.** Version bumps, lockfile updates, the `compat` pin and the
   changelog were each documented somewhere, and none of them in one place.

## Decision

**A book for teaching, a README for proving, a checklist for shipping.**

- **`docs/book/src`** is an mdbook: eight chapters in reading order (introduction, getting
  started, the model, crowds at scale, behaviour, memory, extending, debugging, costs).
  Markdown only, so it reads on a forge without a build; `mdbook build docs/book` is a
  convenience, not a requirement.
- **The README's verification section maps every claim to an artifact.** Not "see the
  performance doc" but "10,000 agents in 9.18 ms, `benches/crowd.rs`, table in
  `docs/dev/tuning/performance.md`". Ten minutes of commands, in order, from a fresh clone.
- **`RELEASING.md` is the only release document**, and it is a command list. The
  tag-triggered `release` CI job repeats it mechanically so the checklist cannot rot silently.
- **`docs/dev/tuning/platforms.md` is the only platform document**, and it states what is *not* claimed
  (no browser runtime test, no second OS until the job existed) as prominently as what is.

## Consequences

- Prose and code drift. The mitigation is structural rather than disciplinary: the book's
  numbers are copied from `docs/dev/tuning/performance.md`, which has a `--check` mode
  (`scripts/scaling_report.py --check`) that fails when the report and the code disagree.
- The audit found a real defect on its first run: with `--no-default-features` the crate did
  not compile, because two modules reached into `memory`-gated code. That is the argument for
  writing the audit as a document with a table rather than a paragraph claiming support, the
  table had to be filled in, and filling it in meant running the builds.
- Shipping benches and examples inside the package increases it from 1.3 MiB to 1.5 MiB and
  removes nine publish warnings. The trade is deliberate: the README tells a reader to run
  `cargo run --example hello_flock`, and that should work from the registry as well as from a
  clone.
- A book can go stale in a way code cannot. The rule that keeps it honest is the one in
  `CONTRIBUTING.md`: a change that alters a measured number updates `docs/dev/tuning/performance.md`
  first, and any prose that quotes it second, in the same commit.

## Alternatives considered

- **A wiki or a hosted site.** Rejected: a second source of truth that no test can see.
- **Doc comments only.** They document the API, which is the one thing a reader can already
  infer from the signatures; they cannot explain the scheduler.
- **A generated book from rustdoc.** Not impossible, but the chapters that matter most (the
  model, crowds at scale) explain decisions, not items.
