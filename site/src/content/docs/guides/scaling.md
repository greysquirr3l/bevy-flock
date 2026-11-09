---
title: Scaling to a million
description: Three regimes, one knob each, and the numbers that bound them.
---

Which knob matters depends on how many NPCs you have. Picking the wrong one is how a crowd engine becomes a physics
demo. The boundaries below are where the cost moves, not where anything breaks.

## Up to 10,000: steering

The frame belongs to steering. Rebuild the spatial grid every frame, keep cognition on a modest budget, and that is the
whole story. The wide kernel is on by default and is worth roughly 19 to 31% of a steering pass at 100k agents depending
on cell size.

Below 10,000, do not enable it expecting the headline. Its advantage scales with density: at 1,000 agents it is a few
percent, and a few percent of a small number is a rounding error with extra code.

## 10,000 to 250,000: the cognition budget

The question stops being "how fast is steering" and becomes "how many NPCs think this tick". The scheduler spends a
virtual-cost budget across candidates chosen by tier and distance. Raise the budget for a smarter crowd at higher cost;
lower it for a bigger one at the same cost.

Measure at your real population, and watch variance rather than the mean. A budget that fits on average will still spike
on the frame where a crowd of NPCs all decide at once.

## Above 250,000: the bands

Distant NPCs stop existing individually. The continuum advances a density and velocity field cheaply and manifests
individuals only near the camera; anchors are always manifested and never folded.

| guarantee | how it is enforced |
| --- | --- |
| population conserved exactly | conservation test over 10,000 strides |
| nothing pops at the boundary | count delta under 2% across transitions |
| a far field cannot leak near | parity test against full simulation |
| anchors never fold | 10 km anchor test, 10,000 ticks |

The continuity suite drives a camera through band transitions for 10,000 ticks and asserts these every tick.

## The measured ceiling

On the reference machine a 1,000,000-inhabitant band frame costs <span class="figure">3.78 ms</span> with the wide
kernel and 4.75 ms scalar, against a 6 ms gate. A 1M-body spatial rebuild costs <span class="figure">4.572 ms</span> at
six workers. Both come with their fixtures in [Performance](/bevy-flock/reference/performance/).

Runnable: `examples/horde_100k.rs`, `examples/ambient_metropolis.rs`, `examples/cross_bench.rs`. Every measurement,
including the optimisations that lost, is in `docs/dev/tuning/performance.md`.
