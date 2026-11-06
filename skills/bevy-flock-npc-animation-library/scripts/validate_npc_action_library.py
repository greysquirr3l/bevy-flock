#!/usr/bin/env python3
"""Validate the shared NPC action GLB produced by retarget_npc_actions.py.

Run through Blender, not CPython:

    blender --background --python \
      skills/bevy-flock-npc-animation-library/scripts/validate_npc_action_library.py -- \
      --asset <game>/assets/npcs/actions/npc_actions.glb

Six things are checked, and each of them has been a real bug somewhere:

1. The file is a glTF 2.0 binary whose declared length matches its size.
2. It contains no meshes. A library that embeds an avatar is a library that will be silently used as
   one, and every NPC will wear the same body.
3. It holds exactly the expected action set — no extra clip exported by accident, no expected clip
   quietly missing because its source name changed upstream.
4. Every animation channel targets a bone that exists on the imported armature.
5. `Hips` translation is zero throughout, because flock owns world position.
6. The animation order is alphabetical, because that order is the index contract
   (`GltfAssetLabel::Animation(n)`), and an index that shifts when a clip is renamed is a runtime bug
   that no type checker can see.
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from pathlib import Path

import bpy

EXPECTED_ACTIONS = frozenset(
    {
        "brace",
        "defeat",
        "hit_flinch",
        "idle",
        "interaction",
        "knockback",
        "locomotion",
        "spawn",
        "turn_left",
        "turn_right",
        "victory",
    }
)
ROOT_LOCATION_PATH = 'pose.bones["Hips"].location'
ROOT_TRANSLATION_EPSILON = 0.0001
BLENDER_ACTION_SUFFIX = "_Armature.002"
GLB_MAGIC = b"glTF"
JSON_CHUNK_TYPE = 0x4E4F534A


def glb_document(path: Path) -> dict[str, object]:
    with path.open("rb") as source:
        magic, version, length = struct.unpack("<4sII", source.read(12))
        if magic != GLB_MAGIC or version != 2 or length != path.stat().st_size:
            raise RuntimeError("invalid glTF binary header")
        chunk_length, chunk_type = struct.unpack("<II", source.read(8))
        if chunk_type != JSON_CHUNK_TYPE:
            raise RuntimeError("missing glTF JSON chunk")
        payload = source.read(chunk_length)
    return json.loads(payload.decode("utf-8"))


def cli_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--asset", required=True, type=Path)
    delimiter = sys.argv.index("--") if "--" in sys.argv else len(sys.argv)
    return parser.parse_args(sys.argv[delimiter + 1 :])


def keyframe_value_is_zero(action: bpy.types.Action) -> bool:
    for fcurve in action.fcurves:
        if fcurve.data_path != ROOT_LOCATION_PATH:
            continue
        for point in fcurve.keyframe_points:
            if abs(point.co.y) > ROOT_TRANSLATION_EPSILON:
                return False
    return True


def action_targets_known_bones(action: bpy.types.Action, bones: set[str]) -> bool:
    for fcurve in action.fcurves:
        if not fcurve.data_path.startswith('pose.bones["'):
            continue
        end = fcurve.data_path.find('"]')
        if end <= len('pose.bones["'):
            return False
        bone_name = fcurve.data_path[len('pose.bones["') : end]
        if bone_name not in bones:
            return False
    return True


def main() -> None:
    arguments = cli_arguments()
    if not arguments.asset.is_file():
        raise RuntimeError(f"missing action library: {arguments.asset}")
    document = glb_document(arguments.asset)
    if document.get("meshes"):
        raise RuntimeError("action library must not embed an avatar mesh")
    bpy.ops.import_scene.gltf(filepath=str(arguments.asset))
    armatures = [object_ for object_ in bpy.context.scene.objects if object_.type == "ARMATURE"]
    if len(armatures) != 1:
        raise RuntimeError(f"expected one action armature, found {len(armatures)}")
    ordered = [
        action.name.removesuffix(BLENDER_ACTION_SUFFIX) for action in bpy.data.actions
    ]
    if set(ordered) != EXPECTED_ACTIONS:
        raise RuntimeError(f"unexpected action set: {sorted(ordered)}")
    expected_order = sorted(EXPECTED_ACTIONS)
    if ordered != expected_order:
        raise RuntimeError(
            f"animation order is not the index contract: file has {ordered}, table says {expected_order}"
        )
    actions = {action.name.removesuffix(BLENDER_ACTION_SUFFIX): action for action in bpy.data.actions}
    bones = {bone.name for bone in armatures[0].data.bones}
    for name, action in actions.items():
        if not action_targets_known_bones(action, bones):
            raise RuntimeError(f"action has an unknown bone target: {name}")
        if not keyframe_value_is_zero(action):
            raise RuntimeError(f"action retains root translation: {name}")
    print(f"validated_action_count={len(actions)}")
    print(f"validated_joint_count={len(bones)}")
    print(f"validated_index_order={','.join(ordered)}")


if __name__ == "__main__":
    main()
