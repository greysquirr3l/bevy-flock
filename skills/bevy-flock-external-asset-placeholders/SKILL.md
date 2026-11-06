---
name: bevy-flock-external-asset-placeholders
description: Create Git-safe placeholder contracts for external NPC models, portraits, and voice lines. Use when approved binaries live in Google Drive or another external package but the repository must document the exact local install paths and stay runnable without them.
---

# External Asset Placeholders

A placeholder turns external media into a deliberate, testable installation rather than an
undocumented missing dependency. Track the contract, ignore the bytes.

## Contract rules

1. Upload and verify the final media externally *before* removing local binaries. A placeholder
   pointing at a package that does not contain the file is worse than no placeholder.
2. Preserve the exact filename the loader uses. A placeholder for `rivet-select.ogg` is
   `rivet-select.ogg.placeholder`, in the same directory.
3. Every placeholder or adjacent README carries: the package location, the expected local filename,
   the destination directory, and what happens when the file is absent.
4. Keep a semantic README in each external asset directory, listing every expected asset in a table.
   The README is the index; the placeholders are the rows.
5. Write narrow `.gitignore` rules for the binary patterns. Never ignore the directory wholesale, or
   the README and the placeholders disappear with the media they describe.
6. Detect complete sets, never partial ones. Half a voice bank is not a degraded experience, it is a
   different bug every session — enable the feature only when the whole set is present.
7. Test both states: clean placeholders, and a complete local install. Missing media may silence
   presentation; it must not block menu flow, spawning, steering, cognition, or tests.

## File patterns

| Asset class | Ignored local media | Tracked contract |
| --- | --- | --- |
| NPC rig | `<slug>.glb` | `.placeholder` plus the directory README |
| Portrait source and derivatives | `portrait_source.png`, `portrait_roster.png`, `portrait_dialogue.png` | Directory README and the asset map |
| Voice line | `<slug>-select.wav`, `<slug>-select.ogg` | `<slug>-select.ogg.placeholder` plus the voice README |
| Shared action library | `npc_actions.glb` | Its README, the licence beside it, and the validator's expected set |
| Future SFX or music | The exact Ogg filename | Same-name `.placeholder` and a category README |

## Placeholder content

```text
bevy-flock external asset placeholder

Expected local runtime asset: <exact-filename>
Package: <verified location>

Copy the approved file into this directory using the exact expected filename.
The game enables the related presentation only when the full approved set is installed.
```

## Verification procedure

1. `git check-ignore -v <expected-binary>` and confirm the media is ignored.
2. `git status --short` and confirm the README and every `.placeholder` are tracked.
3. Add a test that the placeholders exist and that the runtime paths are distinct. Where media may be
   installed locally, write it to accept either zero installed files or a complete set — never a
   partial one.
4. Start the game without media and confirm no navigation failure, no panic, and no simulation
   difference: the same seed produces the same crowd, on the same ticks, whether the meshes are on
   disk or not. That is the property the placeholder exists to protect.
5. With a complete local install, confirm the media resolves and the presentation feature activates.

Do not commit external media unless the user explicitly asks and the licence of every input permits
redistribution.
