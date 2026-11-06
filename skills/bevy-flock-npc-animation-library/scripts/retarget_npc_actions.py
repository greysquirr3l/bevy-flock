#!/usr/bin/env python3
"""Bake selected CC0 Universal Animation Library clips onto a Meshy NPC rig.

Run through Blender, not CPython — this module imports `bpy` at the top and CPython has no such
module:

    blender --background --python skills/bevy-flock-npc-animation-library/scripts/retarget_npc_actions.py -- \
      --ual1 /path/to/UAL1.glb --ual2 /path/to/UAL2.glb \
      --target /path/to/<slug>.glb --output /path/to/actions

The source library carries its own skeleton and the Meshy bodies share a different, shorter humanoid
rig, so clips cannot be reused directly: this transfers local pose deltas bone by bone through
`SOURCE_TO_TARGET_BONES`, sets the target `Hips` translation to zero (flock owns world position), and
exports one GLB holding the skeleton and the curves with no avatar mesh in it.

Blender keeps `bpy.data.actions` in alphabetical order, so the exported animation order is
alphabetical, which is the index contract in SKILL.md. `validate_npc_action_library.py` asserts that
order rather than assuming it; `check_action_contract.py` asserts that this table, the validator's
expected set, and the documented table agree.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import bpy

SOURCE_TO_TARGET_BONES = {
    "pelvis": "Hips",
    "thigh_l": "LeftUpLeg",
    "calf_l": "LeftLeg",
    "foot_l": "LeftFoot",
    "ball_l": "LeftToeBase",
    "thigh_r": "RightUpLeg",
    "calf_r": "RightLeg",
    "foot_r": "RightFoot",
    "ball_r": "RightToeBase",
    "spine_01": "Spine",
    "spine_02": "Spine01",
    "spine_03": "Spine02",
    "clavicle_l": "LeftShoulder",
    "upperarm_l": "LeftArm",
    "lowerarm_l": "LeftForeArm",
    "hand_l": "LeftHand",
    "clavicle_r": "RightShoulder",
    "upperarm_r": "RightArm",
    "lowerarm_r": "RightForeArm",
    "hand_r": "RightHand",
    "neck_01": "neck",
    "Head": "Head",
}

# Output stem, source library, source clip base name. The stem is the runtime clip name and sorts
# into the index order the validator checks, so rename with that in mind: a clip called `z_idle`
# would land at index 10.
SELECTED_ACTIONS = (
    ("brace", "ual2", "Sword_Block"),
    ("defeat", "ual1", "Death01"),
    ("hit_flinch", "ual1", "Hit_Chest"),
    ("idle", "ual1", "Idle_Loop"),
    ("interaction", "ual1", "Interact"),
    ("knockback", "ual2", "Hit_Knockback"),
    ("locomotion", "ual1", "Jog_Fwd_Loop"),
    ("spawn", "ual2", "KipUp"),
    ("turn_left", "ual2", "Turn180_L"),
    ("turn_right", "ual2", "Turn180_R"),
    ("victory", "ual1", "Celebration"),
)

OUTPUT_FILENAME = "npc_actions.glb"


def cli_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ual1", required=True, type=Path)
    parser.add_argument("--ual2", required=True, type=Path)
    parser.add_argument("--target", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    delimiter = sys.argv.index("--") if "--" in sys.argv else len(sys.argv)
    return parser.parse_args(sys.argv[delimiter + 1 :])


def clear_scene() -> None:
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete(use_global=False)
    for action in tuple(bpy.data.actions):
        bpy.data.actions.remove(action)


def import_gltf(path: Path) -> tuple[bpy.types.Object, tuple[bpy.types.Object, ...]]:
    before = set(bpy.context.scene.objects)
    bpy.ops.import_scene.gltf(filepath=str(path))
    imported = tuple(object_ for object_ in bpy.context.scene.objects if object_ not in before)
    armatures = tuple(object_ for object_ in imported if object_.type == "ARMATURE")
    if len(armatures) != 1:
        raise RuntimeError(f"expected one armature in {path.name}, found {len(armatures)}")
    return armatures[0], imported


def action_for(source_armature: bpy.types.Object, base_name: str) -> bpy.types.Action:
    action_name = f"{base_name}_{source_armature.name}"
    action = bpy.data.actions.get(action_name)
    if action is None:
        raise RuntimeError(f"missing source action {action_name}")
    return action


def action_frame_bounds(action: bpy.types.Action) -> tuple[int, int]:
    start, end = action.frame_range
    return int(start), int(end)


def reset_target_pose(target: bpy.types.Object) -> None:
    for pose_bone in target.pose.bones:
        pose_bone.rotation_mode = "QUATERNION"
        pose_bone.matrix_basis.identity()


def bake_action(
    scene: bpy.types.Scene,
    source: bpy.types.Object,
    target: bpy.types.Object,
    source_action: bpy.types.Action,
    output_name: str,
) -> bpy.types.Action:
    if source.animation_data is None:
        source.animation_data_create()
    if target.animation_data is None:
        target.animation_data_create()
    source.animation_data.action = source_action
    target_action = bpy.data.actions.new(name=output_name)
    target.animation_data.action = target_action
    start, end = action_frame_bounds(source_action)
    for frame in range(start, end + 1):
        scene.frame_set(frame)
        for source_name, target_name in SOURCE_TO_TARGET_BONES.items():
            source_pose_bone = source.pose.bones.get(source_name)
            target_pose_bone = target.pose.bones.get(target_name)
            if source_pose_bone is None or target_pose_bone is None:
                continue
            target_pose_bone.matrix_basis = source_pose_bone.matrix_basis.copy()
            if target_name == "Hips":
                # flock owns world position. A clip that also owns it is two authorities for one
                # coordinate, and the drift it produces is indistinguishable from a steering bug.
                target_pose_bone.location = (0.0, 0.0, 0.0)
            target_pose_bone.keyframe_insert(data_path="location", frame=frame)
            target_pose_bone.keyframe_insert(data_path="rotation_quaternion", frame=frame)
            target_pose_bone.keyframe_insert(data_path="scale", frame=frame)
    return target_action


def select_target_armature(target: bpy.types.Object) -> None:
    bpy.ops.object.select_all(action="DESELECT")
    target.select_set(True)
    bpy.context.view_layer.objects.active = target


def remove_imported_objects(objects: tuple[bpy.types.Object, ...]) -> None:
    for object_ in objects:
        bpy.data.objects.remove(object_, do_unlink=True)


def remove_non_runtime_actions(runtime_action_names: set[str]) -> None:
    for action in tuple(bpy.data.actions):
        if action.name not in runtime_action_names:
            bpy.data.actions.remove(action)


def export_action_library(output_path: Path, target: bpy.types.Object) -> None:
    output_path.parent.mkdir(parents=True, exist_ok=True)
    select_target_armature(target)
    bpy.context.scene.frame_set(0)
    target.animation_data.action = None
    bpy.ops.export_scene.gltf(
        filepath=str(output_path),
        export_format="GLB",
        use_selection=True,
        export_animations=True,
        export_nla_strips=False,
        export_force_sampling=True,
        export_frame_range=True,
        export_materials="EXPORT",
    )


def main() -> None:
    arguments = cli_arguments()
    for path in (arguments.ual1, arguments.ual2, arguments.target):
        if not path.is_file():
            raise RuntimeError(f"missing input asset: {path}")
    clear_scene()
    ual1_armature, ual1_objects = import_gltf(arguments.ual1)
    ual2_armature, ual2_objects = import_gltf(arguments.ual2)
    target_armature, _target_objects = import_gltf(arguments.target)
    reset_target_pose(target_armature)
    sources = {"ual1": ual1_armature, "ual2": ual2_armature}

    runtime_action_names = set()
    for output_stem, source_key, source_clip in SELECTED_ACTIONS:
        source_armature = sources[source_key]
        source_action = action_for(source_armature, source_clip)
        bake_action(
            bpy.context.scene,
            source_armature,
            target_armature,
            source_action,
            output_stem,
        )
        runtime_action_names.add(output_stem)
        reset_target_pose(target_armature)
        print(f"baked={output_stem}")

    remove_imported_objects(ual1_objects)
    remove_imported_objects(ual2_objects)
    remove_non_runtime_actions(runtime_action_names)
    export_action_library(arguments.output / OUTPUT_FILENAME, target_armature)
    print(f"exported={OUTPUT_FILENAME}")


if __name__ == "__main__":
    main()
