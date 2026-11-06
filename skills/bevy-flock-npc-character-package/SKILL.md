---
name: bevy-flock-npc-character-package
description: Produce a complete original NPC archetype package for a game built on bevy-flock. Use when creating or replacing an NPC's Meshy model, portraits, bark set, voice lines, handoff package, Git-safe placeholders, and the presentation mapping that makes the archetype appear correctly in the crowd.
---

# NPC Character Package

Deliver a complete original archetype rather than a model that happens to be in a folder. An
archetype is an identity the simulation can reason about and the renderer can draw; half of it is
files, half of it is a mapping.

flock owns the identity and the behaviour. It does not own the mesh, the portrait, or the audio, and
it must not learn about them: the crate's dependency list has no asset loader, and a package that
adds one is a package that broke the library boundary.

## Required outcomes

| Stage | Required outcome |
| --- | --- |
| Identity | Original name, role, and a place in the archetype set that is not a reskin of an existing one. |
| Model | Approved textured Meshy body, verified scale, rigged, with its clip set known. |
| Portraits | Curated source plus deterministic derivatives for the roster card and the dialogue box. |
| Voice | Barks registered in a `BarkTable` under the right `BarkCategory`, plus the recorded lines if the game has audio. |
| Handoff | Approved binaries in the external package; Git-safe placeholders naming exact filenames. |
| Mapping | `VisibleInstance::variant` and `tint` resolve to this archetype; the fallback when media is absent is explicit. |
| Verification | Tests for variant distinctness and absent-media behaviour; a screenshot at crowd distance. |

## Workflow

1. Read the game's archetype table, its asset map, and its art direction before writing anything. A
   seventh archetype that duplicates the fourth is a task that should have been refused.
2. Write a compact brief: silhouette, role, material language, accent colour, portrait personality,
   two or three things they say. Protected character names, dialogue, and visual imitation are out.
   The brief is the artefact every later review is measured against.
3. Apply `../bevy-flock-meshy-assets/SKILL.md`. Concept first, then the model; check bounds and clips
   before anything loads it.
4. Place the model at `assets/npcs/<slug>/<slug>.glb` in the consuming game, with the portrait sources
   beside it. Derive the roster and dialogue sizes mechanically and keep the source, so a later crop
   does not mean another generation.
5. Register the lines. `BarkLine::new(category, text)` for the spoken content, weighted entries where
   one line should be rarer, and the category chosen for the situation that fires it rather than for
   the character's mood. Recorded audio, if the game has it, is keyed by the same slug so the bark and
   the file cannot drift apart.
6. Apply `../bevy-flock-external-asset-placeholders/SKILL.md`. Upload, verify, then commit the
   contract: exact filenames, package location, install instructions, fallback.
7. Apply `../bevy-flock-npc-animation-library/SKILL.md` if this archetype needs clips the shared
   library does not have. Adding a clip means changing the library, its validator, and its table
   together, not forking a per-NPC animation file.
8. Map the archetype in the game's presentation layer. `build_render_plan` fills `VisibleInstance`
   from `NpcIdentity`; the game's renderer reads `variant` to pick the mesh and multiplies `tint`.
   That mapping belongs in the game. Do not add an archetype-to-path lookaside table to flock, and do
   not stringly type it: match the identity exhaustively so a new archetype fails to compile until it
   is drawn.
9. Test both states. With media installed, every archetype's variant resolves to a distinct mesh. With
   no media, the crowd still runs, still steers, still thinks, and the renderer substitutes one
   obvious placeholder — the simulation must not notice that a file is missing.
10. Look at it. Run the game, walk the camera into a crowd of this archetype at the tier that draws
    it, and check silhouette, scale against neighbours, tint range, and whether the portrait still
    reads at its smallest size. A package is not complete because its files exist.

## Local asset contract

In the consuming game, not here:

```text
assets/npcs/<slug>/
├── <slug>.glb                  # ignored local runtime binary
├── portrait_source.png         # ignored full-body source
├── portrait_roster.png         # generated, ignored
├── portrait_dialogue.png       # generated, ignored
├── <slug>-select.ogg           # ignored local voice line, if the game has audio
└── <slug>-select.ogg.placeholder   # tracked installation contract
```

## Completion gate

Do not call the package complete until the upload is verified, the placeholders name exact filenames,
the mapping is exhaustive and compiled, both media states are tested, the crowd screenshot at
drawing distance looks right, and `cargo fmt`, strict clippy, and the workspace tests pass for
whatever changed.
