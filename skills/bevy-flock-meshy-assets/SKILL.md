---
name: bevy-flock-meshy-assets
description: Generate, regenerate, inspect, and review original NPC bodies, heads, and portraits with Meshy for a game built on bevy-flock. Use when deciding how many distinct NPC looks a crowd needs, producing a concept image, running image-to-3D, rigging a body for animation, or reviewing a generated asset before it reaches a renderer.
---

# Meshy NPC Assets

Generate an original NPC through a controlled image-first pipeline, and treat every generated asset
as untrusted until its identity, licensing, geometry, scale, rig, and import behaviour have been
reviewed.

## Decide the variant budget first

This is the step that has no analogue in a six-character game and matters most here. A crowd does not
need a thousand meshes; it needs a few meshes and a great many instances. `build_render_plan` emits
one `VisibleInstance` per drawn NPC, and the only things that can vary per instance are `variant`
(which mesh) and `tint` (multiplied into the material, already derived from the NPC's seed by
`tint_from_seed`).

| Population | Bodies | Heads | Tints | Distinct looks |
| --- | --- | --- | --- | --- |
| 100 | 3–4 | 4–6 | 8 | hundreds, and nobody counts them |
| 10,000 | 4–6 | 6–8 | 16 | tens of thousands of combinations |
| 1,000,000 | as above | as above | 16–24 | the same; nobody is closer than a cell |

Write the budget down before generating anything. Six bodies and eight heads is fourteen generation
tasks; sixty bodies is sixty tasks, sixty reviews, and a draw-call problem you will fix by throwing
the meshes away. Palette variation is nearly free and silhouette variation is not, so spend the tasks
on silhouette: a heavy build, a slight build, a broad build, a stooped build.

`LodTier::Ambient` NPCs never reach the render plan at all, so an LOD chain is only worth building for
the tiers that draw. Budget one lower-detail mesh for `Manifested` and skip LOD generation until a
profile says otherwise.

## Select the correct route

| Need | Preferred route |
| --- | --- |
| Roster card or dialogue portrait | Meshy image-to-image from the approved concept. |
| A new NPC body | Generate a controlled concept image first, then image-to-3D. |
| An existing body that needs animation | Meshy rigging, after confirming the mesh is clean and front-facing. |
| Material correction | Retexture, once the silhouette is approved. Silhouette first, always. |
| Identity correction | Regenerate from the concept with an adjusted prompt; do not patch generated artefacts. |

## Model workflow

1. Write an original prompt under 600 characters: body shape, clothing, materials, silhouette,
   palette, pose. No franchise names, no real people, no "in the style of" a protected character.
2. Request the current Meshy model tier for final work, target GLB output, and ask for a **T-pose**
   when the result will be rigged. A-pose rigs bind badly against a T-pose action library.
3. Generate one concept image and review it before spending a 3D task. Keep the approved concept
   beside the model so a later regeneration has a source.
4. Run image-to-3D with texture enabled, and PBR maps where the NPC needs readable metal, leather, or
   fabric. Matte cloth does not need PBR maps and will not benefit from them.
5. Download only the GLB you need and inspect it locally before anything else touches it: mesh bounds,
   node transforms, animation clips, material count, triangle count. Record the numbers in the asset
   README. Two NPCs at different scales is the most common way a crowd looks wrong, and it is visible
   in a bounds check.
6. Rig the approved body. A Meshy rig does not share a clip index or skeleton layout with any other
   rig, including another Meshy rig; read the model's actual clips and bind the one you verified. See
   `../bevy-flock-npc-animation-library/SKILL.md` for the shared action library.
7. Put the result in the *game's* asset tree, not this repository: `assets/npcs/<slug>/<slug>.glb`.
   Keep generated binaries out of Git unless the licence of every input permits redistribution and
   the repository has agreed to carry the bytes.

## Portrait workflow

1. Start from an approved concept. One local reference, so the identity survives the round trip.
2. Prompt for chest-up framing, a slight three-quarter angle, one adult character, clear facial
   contrast, background separation, and no text, logos, weapons, or full-body framing.
3. Keep one accent colour per archetype, and one shared lighting language across the roster. A crowd
   of NPCs whose portraits disagree about the light reads as a pack of stock images.
4. Save the accepted output as the source and derive the sizes you need once, mechanically, rather
   than asking the model for each size.
5. Review the smallest derivative at its real size. A face that reads at 512 and smears at 96 was
   never approved.

## Review gate

Reject or regenerate on any of these.

| Check | Required result |
| --- | --- |
| Identity | A distinct original character, not a borrowed one, and distinguishable from the other bodies at crowd distance. |
| Scale | Bounds match the other bodies within a few percent. A crowd magnifies scale drift. |
| Silhouette | The archetype is readable as a shape at the distance the tier draws it. |
| Rig | Its own verified clip set, never a guessed index. |
| Materials | No more than the shader actually uses; a dozen materials on one NPC is a batching decision, not an accident. |
| Media hygiene | Approved sources are in the handoff package; Git holds only contracts and documentation. |
| Determinism | Nothing in the asset changes simulation state. Appearance is presentation, and a missing mesh must not change what an NPC does. |

## Handoff

Use `../bevy-flock-external-asset-placeholders/SKILL.md` after curation. A generated asset that exists
only in a temporary directory is a generated asset you will generate again. Upload the approved
deliverables, verify each file exists remotely, then write the placeholder contract naming the exact
filename the loader expects.
