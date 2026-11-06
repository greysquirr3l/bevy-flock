# Skills

Agent-facing playbooks for work that is adjacent to the crate rather than inside it. Each directory
holds a `SKILL.md` with front matter, and a `scripts/` directory where the knowledge can be made
executable.

| Skill | Use it when |
| --- | --- |
| [`bevy-flock-meshy-assets`](bevy-flock-meshy-assets/SKILL.md) | Generating an NPC body, head, or portrait with Meshy, and deciding how many distinct looks a crowd actually needs. |
| [`bevy-flock-npc-character-package`](bevy-flock-npc-character-package/SKILL.md) | Shipping a complete NPC archetype: model, portraits, barks, handoff, and the presentation mapping. |
| [`bevy-flock-external-asset-placeholders`](bevy-flock-external-asset-placeholders/SKILL.md) | A game keeps its media out of Git but must stay runnable, and the repository has to say exactly what to install. |
| [`bevy-flock-npc-animation-library`](bevy-flock-npc-animation-library/SKILL.md) | Building or replacing the shared humanoid action library that every NPC rig plays. |

## Where these came from

These four were ported from the `trapgrid-arsenal` repository (`skills/trapgrid-meshy-operators`,
`trapgrid-character-production`, `trapgrid-external-asset-placeholders`,
`trapgrid-operator-animation-pipeline`, at commit `69b7ce0`), where they were written against a
shipped game: an operator roster, a KayKit fallback rig, a Drive package, and a `main.rs` that owned
presentation. The knowledge transferred; the nouns did not.

| TrapGrid | Here | Why |
| --- | --- | --- |
| operator | NPC archetype | Same idea, no competitive-shooter framing. |
| `crates/trapgrid-bevy/assets/characters/...` | *your game's* `assets/npcs/...` | flock is a library with no renderer and no media. See below. |
| Operator Deck, HUD portrait | roster card, dialogue portrait | flock surfaces the identity; your UI draws it. |
| selection voice line | bark (`BarkTable`, `BarkCategory`) | flock already has a deterministic bark system; a voice line is a bark with audio behind it. |
| `deploy`, `primary_arc` … | `interaction`, and your own extensions | An NPC deploys nothing. The clip set is smaller and honest about it. |
| 24-joint verified rig, UAL1/UAL2 sources | unchanged | The mapping is a property of Meshy's rig and the CC0 source library, not of the game. |

Two things were added in the port rather than translated:

- **A variant budget.** TrapGrid needed six operators with six bespoke meshes. flock draws ten
  thousand NPCs through `VisibleInstance`, where `variant: u16` selects the mesh and `tint` (from
  `tint_from_seed`) recolours it, so the asset question is "how many bodies and heads", not "how many
  characters". `bevy-flock-meshy-assets` starts there.
- **A contract checker.** `bevy-flock-npc-animation-library/scripts/check_action_contract.py` reads
  the retarget script, the validator, and the skill's own table with `ast` and fails if they disagree
  about the clip set or its index order. The original had three copies of that list and nothing
  comparing them.

## The rule that did not survive the port

TrapGrid's skills could assume the game owned assets and the crate owned simulation, because both
were in one repository. Here they are one crate: `bevy-flock` loads no media, opens no files, and
depends on no asset loader. Everything in these skills produces files for the game that consumes the
library, and the seam is `build_render_plan`: it emits `VisibleInstance` values, and your renderer
decides what a variant looks like.

That is not a style preference. Determinism is the crate's headline property, and an asset load that
can fail, block, or arrive late would make a replay depend on a disk.

## Checking them

`cargo` does not see this directory, so CI checks it directly: every Python file here is compiled
with `python3 -m py_compile`, and the animation contract checker runs with its self-test. The Blender
scripts themselves need Blender and are not run in CI — which is why the contract between them is
checked by something that does not.
