# ADR 0036 — Utility scoring alongside trees

**Status:** Accepted
**Date:** 2026-11-05

## Context

Trees are good at "what next" and bad at "which of these eight things is most worth
doing". A town NPC with six plausible actions — trade, gossip, eat, sleep, flee, gawk
— is a utility problem: each option's appeal depends on needs, mood, distance and what
the director just broadcast, and the ordering changes as those move.

Hard-coding that ordering into a `Selector` works until a designer wants to tune it,
and then it becomes a tree of thresholds nobody can reason about.

## Decision

Ship a utility scorer next to the tree engine, in `behavior::utility`.

* Curves (`Linear`, `Inverse`, `Step`, `SmoothStep`, `Logistic` with a steepness
  parameter) map a raw input to a score in `0..=1`. All are monotone and total: no
  input produces `NaN`, a negative score, or a score above one.
* Options compose several considerations by multiplication, the standard utility
  approach. Multiplication is what makes a single failed consideration veto an option.
* Inhibition lets one option suppress another (fear suppressing trade) without
  rewriting either's considerations, and is applied after scoring so the numbers a
  designer sees are the numbers they wrote.
* Scoring compensates for consideration count. Without it, an option with four
  considerations scores systematically lower than one with two, and designers "fix" it
  by inflating weights — which is how a utility system rots. The compensation keeps a
  six-consideration option able to beat a two-consideration option on merit.
* `explain` returns the per-consideration breakdown, because a utility system that
  cannot say *why* an NPC chose something is untunable in practice.

Determinism matters more than smoothness here: the scorer is arithmetic over `f32`
with the same `libm` transcendentals as the rest of the simulation (ADR 0004), so a
replay reproduces the same choice.

## Consequences

* A game can use trees, utility, or both: utility picks the goal, a tree executes it.
  That is the arrangement the archetypes use.
* The compensation term is opinionated. A game that wants raw products can call the
  considerations directly; the default is the one that survives contacts with
  designers.

## Alternatives considered

* **GOAP.** Powerful, and a planner per NPC per decision is a cost this crate's budget
  cannot justify for background crowd members. A game that wants it can implement a
  planner as a leaf.
* **A single weighted sum.** Cheaper, and it cannot express vetoes: an NPC will happily
  "eat" while on fire.
