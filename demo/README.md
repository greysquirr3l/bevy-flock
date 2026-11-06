# Ashfall Verge — a `bevy-flock` playground

A city, a crowd, and four artefacts that prove what happened. Ashfall Verge is a technical
playground for this crate: it exists to answer "what does this library look like when it is a
place?" without asking a reader to take a README's word for it.

![The Ashfall Verge plan](../docs/demo/ashfall-plan.png)

```text
cargo run --release --manifest-path demo/Cargo.toml -- --population 4000 --ticks 900
```

```text
city    6 districts, 2096 blocks, 10 shelters, 10 pylons, 44 stalls over 1800 m
  gate-row       large   r=144   42% of the crowd  (guild)
  aerie          medium  r=90    20% of the crowd  (guild)
  salt-market    medium  r=90    20% of the crowd  (guild)
  low-yard       small   r=68    7% of the crowd
  the-kiln       small   r=68    7% of the crowd
  tannery-row    tiny    r=50    2% of the crowd

crowd   2000 NPCs · 601 ticks · 1.69 ms/tick
tiers   100% reflex / 0% habit / 0% social / 0% deliberate
digest  0x269da17db832c10d (coarse) · crowd 0x55c7a27a6dbad0c1
visitor (1, -38) — the anchor the cognition ladder measures from
```

## What it writes

| Artefact | What it is | Who it is for |
| --- | --- | --- |
| `ashfall.svg` | A top-down plan: districts, ruins, shelters, wayfinding, and every NPC as a dot | A reviewer, in a browser, in one second |
| `ashfall.scene.json` | The render payload: model, position, facing, scale, tint — grouped by model for batching | A renderer, engine-agnostic |
| `ashfall.run.json` | Seed, digest, crowd fingerprint, tier histogram, wall time, per-asset fingerprints | CI, and anyone reading a performance claim |
| `manifest.json` | What shipped: the six models, their heights, their purposes, their provenance | The next person to regenerate an asset |

The demo has no renderer, no window and no asset loader. That is a boundary, not an omission: a
demo that needs a GPU cannot run in CI, and a demo that cannot run in CI stops being evidence.
`ashfall.scene.json` is the `build_render_plan` contract written down — a real consumer for real
GLBs — and a game with a `bevy_render` front end can load it in an afternoon.

## The city

Six settlements on a jittered 34 m grid, joined by 9 m streets, inside 1800 m of ruins. Classes
are large/medium/small/tiny and set the radius, the cap and the guild: `gate-row` is the large
gate settlement and holds 42% of the crowd, `tannery-row` is a 50 m shanty with 2%.

Placement is *keyed*, not sequential: `CityLayout::npc_position(district, NpcId)` draws from
`RngStream::for_npc`, so the same id lands in the same place whatever order the spawner worked in.
`district_for(slot, population)` partitions the crowd exactly — the shares in the table above sum
to the population with no remainder left over.

## The assets

Six models, generated with Meshy from six concept images, then shrunk. All original designs: the
writeup this was pitched from is a setting, not a licence, so nothing here reproduces a character,
a name or a likeness from it.

| Model | Kind | Height | Triangles | Bytes | Purpose |
| --- | --- | --- | --- | --- | --- |
| `gatewarden` | body | 1.80 m | 12,654 | 460 KB | Guards: gate-row, patrols |
| `aerie` | body | 1.80 m | 14,613 | 650 KB | Merchants and nobles: the market, the terrace |
| `scavenger` | body | 1.75 m | 11,506 | 460 KB | Urchins and wanderers: the ruins, the low yard |
| `pylon` | prop | 3.00 m | 6,131 | 270 KB | Wayfinding: one per gate, four at junctions |
| `shelter-door` | prop | 2.50 m | 5,971 | 240 KB | Shelters, in the ruins between settlements |
| `market-stall` | prop | 2.50 m | 7,180 | 340 KB | Stalls, in the market classes |

Three bodies carry five archetypes, and tint plus scale jitter does the rest — the variant budget
argument from the crate documentation, made concrete rather than asserted.

`tools/shrink_glb.py` is the pipeline step that makes them shippable: Meshy's 2K base colour map is
most of a 4 MB file and roughly eight times more texture than a 12,000-triangle prop can show. The
script re-encodes the images, re-lays the binary chunk and leaves geometry, accessors and indices
byte-identical. 22.9 MB became 2.4 MB:

```text
  aerie.glb               4.62 MB ->  0.65 MB   14613 tris  1.80 m tall
  gatewarden.glb          4.36 MB ->  0.46 MB   12654 tris  1.80 m tall
  scavenger.glb           3.74 MB ->  0.46 MB   11506 tris  1.75 m tall
  market-stall.glb        3.46 MB ->  0.34 MB    7180 tris  2.50 m tall
  pylon.glb               3.32 MB ->  0.27 MB    6131 tris  3.00 m tall
  shelter-door.glb        3.36 MB ->  0.24 MB    5971 tris  2.50 m tall
```

`tests/assets.rs` is the gate: every promised file is opened, its container and version checked,
its geometry confirmed, its animation absence asserted and its size held to a budget. A
regenerated asset that skips the shrink step fails there.

## Two things the demo had to learn the hard way

Both are recorded because they are the kind of thing a "playground" is for.

**A crowd with cohesion is a flock.** With the default weights, everyone walks to the same
intersection; the first build of this demo produced a city with a single dense dot in the middle of
it. Cohesion off, alignment low, seek on: a street holds its districts.

**A headless app has no clock.** `App::new()` does not include `TimePlugin`, and `integrate_flock`
advances positions by the frame delta, so the crowd stood perfectly still — placed, drawn, and
never moving. `tests/playground.rs::the_crowd_walks` is the test that caught it, and it is still
there: half a second of simulated time is not long enough to see anyone walk, which is why it runs
300 ticks rather than 30.

## What the numbers mean

The tier histogram reads 100% reflex at these populations, and that is correct rather than
disappointing: "reflex" is the *highest* fidelity band, and below the manifest band there is no
reason to demote anyone. Push the population past it and the ladder starts folding — the library's
benchmarks cover that path; this demo is sized to stay readable.

`ms_per_tick` is one number on one machine with a debug-friendly configuration: it is not the
kernel benchmark, and `docs/performance.md` is where the measured performance of the steering road
lives. What `ashfall.run.json` gives you is the same measurement *for the run you just made*, next
to the seed and the digest, so a change in this demo's cost is attributable.

The first `districts` line in the summary is longer than the population check deserves: the
`digest` is deliberately coarse (population, tiers, mood, the tick) and matches across seeds, so
`crowd_fingerprint` hashes quantised positions to the millimetre for the artefacts.

## Layout

```text
demo/
  src/app.rs        Playground: build, place, tick, read back   <- tests drive this
  src/city.rs       Districts, blocks, shelters, pylons, stalls, keyed placement
  src/assets.rs     The model table, the body mapping, the GLB probe
  src/scene.rs      Engine-agnostic instances + crowd fingerprint
  src/plan.rs       The SVG plan
  src/report.rs     run.json
  src/json.rs       A 90-line writer (no serde: the demo writes four small documents)
  src/main.rs       Flags and artefacts
  tests/            assets.rs (the gate), playground.rs (determinism, walking, placement)
  tools/            shrink_glb.py
  assets/           npcs/, props/, concepts/
```

The crate is not in the root workspace, for the same reason `compat/` is not: a demo should not be
able to break the library's build, and the library should not pay for the demo's dependencies.
