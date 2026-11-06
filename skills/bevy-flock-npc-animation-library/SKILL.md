---
name: bevy-flock-npc-animation-library
description: Build or replace the shared humanoid action library that every bevy-flock NPC rig plays, from CC0 Universal Animation Library or compatible source clips. Use when inspecting animation archives, validating licences and root-motion policy, retargeting actions onto the shared Meshy rig in Blender, rebuilding npc_actions.glb, or wiring clip indices into a Bevy animation graph.
---

# NPC Action Library

Produce one **validated shared action library**, not a folder of per-NPC clips. Every NPC rig plays
the same eleven actions, because a crowd is a hundred identical skeletons in different poses, and ten
thousand copies of a clip set is not an asset pipeline.

Animation is presentation. It reads simulation state and never writes it: no clip, no root motion, and
no completion callback may change an NPC's position, behaviour, or cognition.

## Preconditions

1. Confirm the source licence permits derived runtime assets, and keep it beside the derived file. Do
   not assume that Unity, Unreal, FBX, GLB, or a marketplace download is redistributable.
2. Prefer non-root-motion sources, and reject `_RM` variants. flock owns world position; a clip that
   also owns it produces two authorities for one coordinate, which is a bug that looks like drift.
3. Inspect both skeletons before retargeting. Meshy's humanoid rig and the CC0 Universal Animation
   Library rig do not share a hierarchy — the mapping in the script is explicit for that reason, and
   clip reuse without retargeting is not a shortcut, it is a broken import.
4. Obtain one locally installed, ignored Meshy target body. Never commit avatars, source archives,
   Blender files, FBX exports, or reference images in order to rebuild clips.

## Workflow

1. Inventory every supplied archive without executing it. Read the licence and readme; record format,
   clip count, root-motion variants, and coverage. Write it down — the review gate needs something to
   compare against.
2. Inspect both skeletons and confirm the joint lists. All NPC bodies must share one target hierarchy
   before a single library can serve them; a second rig is a second library and a second set of
   indices.
3. Confirm the state vocabulary below. Adding a clip is a change to the script, the validator, and the
   table in this file, together. `scripts/check_action_contract.py` fails if they disagree, which is
   the only reason three copies of one list are allowed to exist.
4. Edit `scripts/retarget_npc_actions.py` only when a source or target mapping genuinely changes. Keep
   `SOURCE_TO_TARGET_BONES` explicit, transfer mapped local pose deltas, zero the `Hips` translation,
   and export skeleton plus animation only.
5. Run Blender in batch mode from the repository root:

   ```bash
   blender --background --python skills/bevy-flock-npc-animation-library/scripts/retarget_npc_actions.py -- \
     --ual1 /path/to/UAL1.glb \
     --ual2 /path/to/UAL2.glb \
     --target /path/to/<slug>.glb \
     --output /tmp/npc-actions
   ```

6. Copy only `/tmp/npc-actions/npc_actions.glb` into the game's `assets/npcs/actions/`, beside the
   source licence. Do not embed a target avatar mesh in this file; the library is a skeleton and
   curves.
7. Validate before anything loads it:

   ```bash
   blender --background --python skills/bevy-flock-npc-animation-library/scripts/validate_npc_action_library.py -- \
     --asset <game>/assets/npcs/actions/npc_actions.glb
   ```

   The gate requires the exact action set, target-only bone channels, zero `Hips` translation, no
   glTF `meshes`, valid glTF 2.0, and the alphabetical index order the table promises.
8. Keep the runtime's clip indices honest. `GltfAssetLabel::Animation(n)` takes an index, so the index
   is a contract: keep a README beside the asset recording the order, and read the graph nodes in that
   order rather than by looking them up by name at runtime.
9. Map simulation state to clips in the game's animation layer, in one place: a match on the NPC's
   behaviour state and presentation requests, exhaustive rather than a string fallback. Higher
   priority one-shots preempt loops; a completed one-shot returns to locomotion or idle.
10. Run `cargo fmt --all`, strict clippy, and the workspace tests. Then watch it: a Bevy panic, an
    action-graph error, a missing clip, or a target-resolution warning is blocking, not cosmetic.

## Stable action library contract

Index order is alphabetical, which is the order Blender's action collection exports in; the validator
asserts it rather than trusting it.

| Index | Clip | Source |
| ---: | --- | --- |
| 0 | `brace` | `ual2` `Sword_Block` |
| 1 | `defeat` | `ual1` `Death01` |
| 2 | `hit_flinch` | `ual1` `Hit_Chest` |
| 3 | `idle` | `ual1` `Idle_Loop` |
| 4 | `interaction` | `ual1` `Interact` |
| 5 | `knockback` | `ual2` `Hit_Knockback` |
| 6 | `locomotion` | `ual1` `Jog_Fwd_Loop` |
| 7 | `spawn` | `ual2` `KipUp` |
| 8 | `turn_left` | `ual2` `Turn180_L` |
| 9 | `turn_right` | `ual2` `Turn180_R` |
| 10 | `victory` | `ual1` `Celebration` |

Eleven is deliberate. A crowd needs to idle, move, turn, react, be hit, die, arrive, celebrate, and
interact with something; it does not need a combat move set per archetype. An archetype that needs
more gets a new clip in this table with its source recorded, not a private animation file.

Check the three copies agree without launching Blender:

```bash
python3 skills/bevy-flock-npc-animation-library/scripts/check_action_contract.py
```

## Safety rules

- Animation must not mutate simulation state. No clip, root motion, or completion event writes to
  position, behaviour, cognition, or anything `flock` owns.
- Presentation must not leak hidden state. An animation choice that reveals what an NPC is about to do
  is a gameplay change wearing a costume.
- Keep source archives out of tracked runtime paths unless the licence permits redistribution.
- Do not commit a derived action asset until the Blender validation gate has passed and the game has
  played it.

## Completion gate

Publish only when the scripts are executable, the derived GLB passes validation, every target body is
confirmed compatible with the shared skeleton, the game plays each clip through typed state
transitions, the contract checker passes, and the commit carries the licence and the index
documentation.
