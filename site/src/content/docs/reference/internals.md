---
title: Where the internals live
description: The map of the repository's development documentation, and why it is not on this site.
---

This site is the adoption path: install, first crowd, concepts, guides. The material a maintainer needs, why a decision
was made, what it cost, which alternatives were measured and rejected, lives in the repository under `docs/dev/`, and is
linked rather than republished so there is exactly one copy of every number.

```text
docs/
├── book/            the long-form guide (mdBook), older and deeper than this site
├── demo/            terminal captures and images used by the demo crates
├── dev/
│   ├── adr/         architecture decision records, 0001 → 0062, indexed
│   ├── lint/        the lint policy and the stable-lint drift ledger
│   ├── meta/        1.0 readiness, release checklists
│   └── tuning/      performance journals, memory, gates, benchmarks, research, playbooks
└── images/
```

## Where to start, by question

| question | file |
| --- | --- |
| Why is the spatial build shaped like this? | `docs/dev/adr/0057-fused-count-and-scan.md`, `0059-worker-pool-ownership.md` |
| Why is there no 5× speedup? | `docs/dev/adr/0060-build-5x-gate.md` |
| What did the last tuning pass actually change? | `docs/dev/tuning/performance.md` |
| How much memory per agent, and why? | `docs/dev/tuning/memory.md` |
| What are the nine continuum gates? | `docs/dev/tuning/gates/continuum.md` |
| How do I compare against Godot or Unreal? | `docs/dev/tuning/benchmarks/cross-engine.md`, harnesses in `benchmarks/` |
| What does 1.0 require? | `docs/dev/meta/1.0-readiness.md`, `release-checklist-1.0.md` |

## The rule this layout encodes

If a document tells a *user* what to do, it belongs on this site. If it tells a *maintainer* why the code is the way it
is, it belongs in `docs/dev/`. Neither ships in the crates.io tarball: the `exclude` list in both manifests keeps the
packages to code and the README.
