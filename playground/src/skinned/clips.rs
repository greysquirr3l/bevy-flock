//! The clips, authored in code: idle, walk, run, and four one-shot gestures.
//!
//! No glTF, no asset pipeline, no download step. Each clip is a handful of keyframes on the three
//! animated bones, which is enough to make the difference between standing, walking and running
//! legible in a screenshot — and enough for the blend to have something to blend.
//!
//! # Why the periods are what they are
//!
//! A walk cycle is two steps: the same leg is forward again after a full period. The example's
//! stride is 1.5 m, so a body at 1.4 m/s covers a stride in 1.07 s and the walk clip is authored at
//! that length. Run is authored shorter because a run is faster, not because the clip plays faster:
//! [`bevy_flock::anim::apply_locomotion_blend`] also scales playback rate by speed over the profile's
//! stride, so the clip's own period is the tempo a body at the profile's walk speed would show.

use bevy_animation::animation_curves::AnimationCurve;
use bevy_animation::animation_curves::{AnimatableCurve, AnimatableKeyframeCurve};
use bevy_animation::graph::AnimationGraph;
use bevy_animation::graph::AnimationNodeIndex;
use bevy_animation::{AnimationClip, AnimationTargetId, animated_field};
use bevy_asset::{Assets, Handle};
use bevy_math::{Quat, Vec3};
use bevy_transform::components::Transform;

/// Why a clip could not be built.
///
/// The keyframes are constants in this module, so a failure here is a programming error rather than
/// bad input — but it is still reported rather than panicking, because a library that decides for a
/// game that its process should die is a library a game cannot ship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipError {
    /// A keyframe list had fewer than two keys, or keys out of order.
    Keyframes,
}

/// The node indices of the clips this example installs, and the handle of the graph holding them.
#[derive(Debug, Clone, Copy)]
pub struct RigClips {
    /// The standing clip.
    pub idle: AnimationNodeIndex,
    /// The walking clip.
    pub walk: AnimationNodeIndex,
    /// The running clip.
    pub run: AnimationNodeIndex,
    /// A gesture: a shrug that reads as a wave from a distance.
    pub wave: AnimationNodeIndex,
    /// A gesture: a nod.
    pub nod: AnimationNodeIndex,
    /// A gesture: a flinch backwards.
    pub flinch: AnimationNodeIndex,
    /// A gesture: a bark, head back.
    pub bark: AnimationNodeIndex,
}

/// The graph and the clip nodes inside it, ready to hand to a crowd.
#[derive(Debug, Clone)]
pub struct ClipLibrary {
    /// The graph asset every body points at.
    pub graph: Handle<AnimationGraph>,
    /// The node indices inside it.
    pub clips: RigClips,
}

/// Turn a list of keyframes into a rotation curve on one bone.
fn rotation_curve(keys: &[(f32, Quat)]) -> Result<impl AnimationCurve, ClipError> {
    let curve =
        AnimatableKeyframeCurve::new(keys.iter().copied()).map_err(|_| ClipError::Keyframes)?;
    Ok(AnimatableCurve::new(
        animated_field!(Transform::rotation),
        curve,
    ))
}

/// Turn a list of keyframes into a translation curve, for the hips' bob.
fn translation_curve(keys: &[(f32, Vec3)]) -> Result<impl AnimationCurve, ClipError> {
    let curve =
        AnimatableKeyframeCurve::new(keys.iter().copied()).map_err(|_| ClipError::Keyframes)?;
    Ok(AnimatableCurve::new(
        animated_field!(Transform::translation),
        curve,
    ))
}

/// The animation target id of one joint name.
fn target(name: &str) -> AnimationTargetId {
    AnimationTargetId::from_iter([name])
}

/// A yaw, pitch and roll in radians as a quaternion.
fn euler(yaw: f32, pitch: f32, roll: f32) -> Quat {
    Quat::from_euler(bevy_math::EulerRot::YXZ, yaw, pitch, roll)
}

/// Add a looping rotation to one bone.
fn add_loop_rotation(
    clip: &mut AnimationClip,
    joint: &str,
    keys: &[(f32, Quat)],
) -> Result<(), ClipError> {
    clip.add_curve_to_target(target(joint), rotation_curve(keys)?);
    Ok(())
}

/// The idle clip: a body standing still is not a statue.
///
/// Four seconds of weight shift, which is slow enough to read as breathing rather than as a sway, and
/// a head that drifts a degree and a half — the movement that tells a viewer the crowd is alive even
/// The idle clip: a body standing still is not a statue.
///
/// Four seconds of weight shift, which is slow enough to read as breathing rather than as a sway, a
/// head that drifts a degree and a half, and arms that follow the chest a beat late. No leg motion:
/// a standing body's feet stay planted, which is the difference between idle and a walk at zero
/// speed.
fn idle_clip() -> Result<AnimationClip, ClipError> {
    let mut clip = AnimationClip::default();
    add_loop_rotation(
        &mut clip,
        "torso",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (1.0, euler(0.0, 0.010, 0.020)),
            (2.0, euler(0.0, 0.0, 0.0)),
            (3.0, euler(0.0, -0.010, -0.020)),
            (4.0, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "head",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (1.3, euler(0.05, -0.015, 0.0)),
            (2.6, euler(0.0, 0.0, 0.0)),
            (3.9, euler(-0.05, 0.015, 0.0)),
            (5.2, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "arm_left",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (2.0, euler(0.0, 0.03, 0.0)),
            (4.0, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "arm_right",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (2.2, euler(0.0, 0.03, 0.0)),
            (4.4, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    Ok(clip)
}

/// A walk: two steps a cycle, with the hips bobbing twice and the shoulders counter-rotating.
///
/// The counter-rotation is the detail that makes a walk read as a walk: the pelvis turns with the
/// A walk: two steps a cycle, with the hips bobbing twice and the shoulders counter-rotating.
///
/// The legs are the clip. Everything else is what a body does *around* a step: the pelvis turns with
/// the leading leg, the chest turns against it, the arms swing opposite the legs, and the hips drop
/// twice a cycle because a walk's lowest point is when both feet are down.
fn walk_clip() -> Result<AnimationClip, ClipError> {
    let mut clip = AnimationClip::default();
    // Legs: a straight sinusoid a half cycle apart. Positive pitch swings a limb forward, and the
    // forward is the body's own -Z, so a body walking along its facing steps where it looks.
    add_loop_rotation(
        &mut clip,
        "leg_left",
        &[
            (0.0, euler(0.0, 0.42, 0.0)),
            (0.25, euler(0.0, 0.0, 0.0)),
            (0.5, euler(0.0, -0.42, 0.0)),
            (0.75, euler(0.0, 0.0, 0.0)),
            (1.0, euler(0.0, 0.42, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "leg_right",
        &[
            (0.0, euler(0.0, -0.42, 0.0)),
            (0.25, euler(0.0, 0.0, 0.0)),
            (0.5, euler(0.0, 0.42, 0.0)),
            (0.75, euler(0.0, 0.0, 0.0)),
            (1.0, euler(0.0, -0.42, 0.0)),
        ],
    )?;
    // Arms swing against the legs, at about two thirds of the amplitude: an arm that swings as far as
    // a leg reads as a march.
    add_loop_rotation(
        &mut clip,
        "arm_left",
        &[
            (0.0, euler(0.0, -0.30, 0.0)),
            (0.25, euler(0.0, 0.0, 0.0)),
            (0.5, euler(0.0, 0.30, 0.0)),
            (0.75, euler(0.0, 0.0, 0.0)),
            (1.0, euler(0.0, -0.30, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "arm_right",
        &[
            (0.0, euler(0.0, 0.30, 0.0)),
            (0.25, euler(0.0, 0.0, 0.0)),
            (0.5, euler(0.0, -0.30, 0.0)),
            (0.75, euler(0.0, 0.0, 0.0)),
            (1.0, euler(0.0, 0.30, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "hips",
        &[
            (0.0, euler(0.08, 0.0, 0.0)),
            (0.25, euler(0.0, 0.0, 0.0)),
            (0.5, euler(-0.08, 0.0, 0.0)),
            (0.75, euler(0.0, 0.0, 0.0)),
            (1.0, euler(0.08, 0.0, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "torso",
        &[
            (0.0, euler(-0.06, 0.0, 0.025)),
            (0.25, euler(0.0, 0.0, 0.0)),
            (0.5, euler(0.06, 0.0, -0.025)),
            (0.75, euler(0.0, 0.0, 0.0)),
            (1.0, euler(-0.06, 0.0, 0.025)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "head",
        &[
            (0.0, euler(0.03, 0.0, 0.0)),
            (0.5, euler(-0.03, 0.0, 0.0)),
            (1.0, euler(0.03, 0.0, 0.0)),
        ],
    )?;
    clip.add_curve_to_target(
        target("hips"),
        translation_curve(&[
            (0.0, Vec3::new(0.0, 0.915, 0.0)),
            (0.25, Vec3::new(0.0, 0.94, 0.0)),
            (0.5, Vec3::new(0.0, 0.915, 0.0)),
            (0.75, Vec3::new(0.0, 0.94, 0.0)),
            (1.0, Vec3::new(0.0, 0.915, 0.0)),
        ])?,
    );
    Ok(clip)
}

/// A run: the same channels, wider and quicker, with the whole body leaning forward.
fn run_clip() -> Result<AnimationClip, ClipError> {
    let mut clip = AnimationClip::default();
    add_loop_rotation(
        &mut clip,
        "leg_left",
        &[
            (0.0, euler(0.0, 0.70, 0.0)),
            (0.16, euler(0.0, 0.0, 0.0)),
            (0.32, euler(0.0, -0.70, 0.0)),
            (0.48, euler(0.0, 0.0, 0.0)),
            (0.64, euler(0.0, 0.70, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "leg_right",
        &[
            (0.0, euler(0.0, -0.70, 0.0)),
            (0.16, euler(0.0, 0.0, 0.0)),
            (0.32, euler(0.0, 0.70, 0.0)),
            (0.48, euler(0.0, 0.0, 0.0)),
            (0.64, euler(0.0, -0.70, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "arm_left",
        &[
            (0.0, euler(0.12, -0.62, 0.0)),
            (0.16, euler(0.12, 0.0, 0.0)),
            (0.32, euler(0.12, 0.62, 0.0)),
            (0.48, euler(0.12, 0.0, 0.0)),
            (0.64, euler(0.12, -0.62, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "arm_right",
        &[
            (0.0, euler(-0.12, 0.62, 0.0)),
            (0.16, euler(-0.12, 0.0, 0.0)),
            (0.32, euler(-0.12, -0.62, 0.0)),
            (0.48, euler(-0.12, 0.0, 0.0)),
            (0.64, euler(-0.12, 0.62, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "hips",
        &[
            (0.0, euler(0.14, 0.0, 0.0)),
            (0.16, euler(0.0, 0.0, 0.0)),
            (0.32, euler(-0.14, 0.0, 0.0)),
            (0.48, euler(0.0, 0.0, 0.0)),
            (0.64, euler(0.14, 0.0, 0.0)),
        ],
    )?;
    // A run leans: negative pitch takes the chest forward over the leading foot.
    add_loop_rotation(
        &mut clip,
        "torso",
        &[
            (0.0, euler(-0.10, -0.16, 0.05)),
            (0.16, euler(0.0, -0.16, 0.0)),
            (0.32, euler(0.10, -0.16, -0.05)),
            (0.48, euler(0.0, -0.16, 0.0)),
            (0.64, euler(-0.10, -0.16, 0.05)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "head",
        &[
            (0.0, euler(0.04, 0.12, 0.0)),
            (0.32, euler(-0.04, 0.12, 0.0)),
            (0.64, euler(0.04, 0.12, 0.0)),
        ],
    )?;
    clip.add_curve_to_target(
        target("hips"),
        translation_curve(&[
            (0.0, Vec3::new(0.0, 0.90, 0.0)),
            (0.16, Vec3::new(0.0, 0.965, 0.0)),
            (0.32, Vec3::new(0.0, 0.90, 0.0)),
            (0.48, Vec3::new(0.0, 0.965, 0.0)),
            (0.64, Vec3::new(0.0, 0.90, 0.0)),
        ])?,
    );
    Ok(clip)
}

/// A one-shot gesture: the right arm comes up and waves, twice, and comes down.
///
/// Roll raises the arm; the small pitch on the way up is what stops it reading as a semaphore. The
/// left arm stays where the blend left it, which is the point of a per-kind clip rather than one
/// "gesture" animation that moves the whole body.
fn wave_clip() -> Result<AnimationClip, ClipError> {
    let mut clip = AnimationClip::default();
    add_loop_rotation(
        &mut clip,
        "arm_right",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (0.2, euler(0.0, -0.10, 2.10)),
            (0.35, euler(0.0, -0.10, 1.80)),
            (0.5, euler(0.0, -0.10, 2.25)),
            (0.65, euler(0.0, -0.10, 1.80)),
            (0.8, euler(0.0, -0.10, 2.10)),
            (0.95, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "torso",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (0.2, euler(0.10, 0.0, -0.06)),
            (0.8, euler(0.10, 0.0, -0.06)),
            (0.95, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "head",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (0.3, euler(0.18, 0.05, 0.0)),
            (0.95, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    Ok(clip)
}

/// A one-shot gesture: a nod, twice.
fn nod_clip() -> Result<AnimationClip, ClipError> {
    let mut clip = AnimationClip::default();
    add_loop_rotation(
        &mut clip,
        "head",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (0.15, euler(0.0, -0.28, 0.0)),
            (0.3, euler(0.0, 0.04, 0.0)),
            (0.45, euler(0.0, -0.28, 0.0)),
            (0.6, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    Ok(clip)
}

/// A one-shot gesture: a flinch, which is the torso leaning away before the head catches up.
fn flinch_clip() -> Result<AnimationClip, ClipError> {
    let mut clip = AnimationClip::default();
    add_loop_rotation(
        &mut clip,
        "torso",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (0.1, euler(0.0, 0.18, 0.0)),
            (0.35, euler(0.0, -0.04, 0.0)),
            (0.6, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    Ok(clip)
}

/// A one-shot gesture: a bark, head back and shoulders open.
fn bark_clip() -> Result<AnimationClip, ClipError> {
    let mut clip = AnimationClip::default();
    add_loop_rotation(
        &mut clip,
        "head",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (0.12, euler(0.0, 0.30, 0.0)),
            (0.4, euler(0.0, 0.30, 0.0)),
            (0.7, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    add_loop_rotation(
        &mut clip,
        "torso",
        &[
            (0.0, euler(0.0, 0.0, 0.0)),
            (0.12, euler(0.0, 0.10, 0.0)),
            (0.7, euler(0.0, 0.0, 0.0)),
        ],
    )?;
    Ok(clip)
}

/// The clip assets, one handle each, before a graph has been built around them.
#[derive(Debug, Clone)]
pub struct ClipHandles {
    /// The standing clip.
    pub idle: Handle<AnimationClip>,
    /// The walking clip.
    pub walk: Handle<AnimationClip>,
    /// The running clip.
    pub run: Handle<AnimationClip>,
    /// The wave gesture.
    pub wave: Handle<AnimationClip>,
    /// The nod gesture.
    pub nod: Handle<AnimationClip>,
    /// The flinch gesture.
    pub flinch: Handle<AnimationClip>,
    /// The bark gesture.
    pub bark: Handle<AnimationClip>,
}

/// Add every clip to the store and hand back its handle.
///
/// Separate from [`build_graph`] because a game builds its clips and its graph from different places:
/// the clip store is what an asset loader fills, and the graph is what the game assembles once the
/// clips exist.
///
/// # Errors
///
/// [`ClipError::Keyframes`] if a constant keyframe list in this module is malformed.
pub fn build_clip_assets(clips: &mut Assets<AnimationClip>) -> Result<ClipHandles, ClipError> {
    Ok(ClipHandles {
        idle: clips.add(idle_clip()?),
        walk: clips.add(walk_clip()?),
        run: clips.add(run_clip()?),
        wave: clips.add(wave_clip()?),
        nod: clips.add(nod_clip()?),
        flinch: clips.add(flinch_clip()?),
        bark: clips.add(bark_clip()?),
    })
}

/// Build the graph around clips that already exist.
///
/// The graph is one asset shared by the whole crowd: two thousand bodies pointing at two thousand
/// copies of the same graph would be two thousand copies of the same numbers.
pub fn build_graph(graphs: &mut Assets<AnimationGraph>, handles: &ClipHandles) -> ClipLibrary {
    let mut graph = AnimationGraph::new();
    let root = graph.root;
    let mut add = |clip: Handle<AnimationClip>| graph.add_clip(clip, 1.0, root);
    let clips = RigClips {
        idle: add(handles.idle.clone()),
        walk: add(handles.walk.clone()),
        run: add(handles.run.clone()),
        wave: add(handles.wave.clone()),
        nod: add(handles.nod.clone()),
        flinch: add(handles.flinch.clone()),
        bark: add(handles.bark.clone()),
    };
    ClipLibrary {
        graph: graphs.add(graph),
        clips,
    }
}

/// Build every clip and the graph that holds them.
///
/// The graph is one asset shared by the whole crowd: two thousand bodies with two thousand copies of
/// the same graph would be two thousand copies of the same numbers.
pub fn build_library(
    clips: &mut Assets<AnimationClip>,
    graphs: &mut Assets<AnimationGraph>,
) -> Result<ClipLibrary, ClipError> {
    let mut graph = AnimationGraph::new();
    let root = graph.root;

    let mut add = |clip: AnimationClip| graph.add_clip(clips.add(clip), 1.0, root);

    let idle = add(idle_clip()?);
    let walk = add(walk_clip()?);
    let run = add(run_clip()?);
    let wave = add(wave_clip()?);
    let nod = add(nod_clip()?);
    let flinch = add(flinch_clip()?);
    let bark = add(bark_clip()?);

    Ok(ClipLibrary {
        graph: graphs.add(graph),
        clips: RigClips {
            idle,
            walk,
            run,
            wave,
            nod,
            flinch,
            bark,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{ClipError, build_library, idle_clip, nod_clip, rotation_curve, walk_clip};
    use bevy_animation::AnimationClip;
    use bevy_asset::Assets;
    use std::collections::HashSet;

    #[test]
    fn every_clip_animates_at_least_one_bone_and_has_a_duration() {
        for build in [
            idle_clip as fn() -> Result<AnimationClip, ClipError>,
            walk_clip,
            nod_clip,
        ] {
            let clip = build().unwrap_or_else(|error| unreachable!("{error:?}"));
            assert!(
                !clip.curves().is_empty(),
                "a clip with no curves animates nothing"
            );
            assert!(
                clip.duration() > 0.0,
                "a clip with no duration never advances"
            );
        }
    }

    #[test]
    fn every_curve_spans_the_clip_from_zero() {
        // A curve whose domain starts after zero leaves the target at whatever the previous clip left
        // there for the first frames of the cycle, which reads as a twitch at the loop point.
        for build in [
            idle_clip as fn() -> Result<AnimationClip, ClipError>,
            walk_clip,
            nod_clip,
        ] {
            let clip = build().unwrap_or_else(|error| unreachable!("{error:?}"));
            let duration = clip.duration();
            for (target, curves) in clip.curves() {
                for curve in curves {
                    let domain = curve.0.domain();
                    assert!(
                        domain.start().abs() < 1.0e-6,
                        "clip {target:?} starts at {}",
                        domain.start()
                    );
                    assert!(domain.end() <= duration + 1.0e-6);
                }
            }
        }
    }

    #[test]
    fn the_walk_moves_the_legs_and_the_arms() {
        // The channel list is the animation. A walk that animates the hips alone is a body that
        // turns as it slides, which is what the first cut of this example did.
        let clip = walk_clip().unwrap_or_else(|error| unreachable!("{error:?}"));
        let targets: Vec<bevy_animation::AnimationTargetId> =
            clip.curves().keys().copied().collect();
        for joint in [
            super::super::rig::Joint::LegLeft,
            super::super::rig::Joint::LegRight,
            super::super::rig::Joint::ArmLeft,
            super::super::rig::Joint::ArmRight,
        ] {
            assert!(
                targets.contains(&super::target(joint.name())),
                "the walk never animates {}",
                joint.name()
            );
        }
    }

    #[test]
    fn a_curve_needs_two_keyframes() {
        assert_eq!(rotation_curve(&[]).err(), Some(ClipError::Keyframes));
        assert!(rotation_curve(&[(0.0, bevy_math::Quat::IDENTITY)]).is_err());
        assert!(
            rotation_curve(&[
                (0.0, bevy_math::Quat::IDENTITY),
                (1.0, bevy_math::Quat::IDENTITY)
            ])
            .is_ok()
        );
    }

    #[test]
    fn the_library_installs_seven_distinct_nodes() {
        let mut clips = Assets::<AnimationClip>::default();
        let mut graphs = Assets::default();
        let library = build_library(&mut clips, &mut graphs)
            .unwrap_or_else(|error| unreachable!("{error:?}"));
        let nodes = [
            library.clips.idle,
            library.clips.walk,
            library.clips.run,
            library.clips.wave,
            library.clips.nod,
            library.clips.flinch,
            library.clips.bark,
        ];
        let unique: HashSet<_> = nodes.iter().collect();
        assert_eq!(unique.len(), nodes.len(), "two gestures share a node");
        assert_eq!(clips.len(), 7);
        assert_eq!(graphs.len(), 1);
        assert_ne!(library.clips.idle, library.clips.walk);
    }

    #[test]
    fn the_walk_is_longer_than_the_run_because_a_run_is_faster_not_shorter() {
        let walk = walk_clip().unwrap_or_else(|error| unreachable!("{error:?}"));
        let run = super::run_clip().unwrap_or_else(|error| unreachable!("{error:?}"));
        assert!(walk.duration() > run.duration());
    }
}
