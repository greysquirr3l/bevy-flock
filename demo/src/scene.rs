//! The scene: what a renderer would draw, in the order a renderer wants it.
//!
//! `build_render_plan` in the library turns ECS state into `VisibleInstance` values — where each
//! NPC is, which way it faces, a tint, and a `variant` number it deliberately refuses to interpret.
//! This module is the other half of that contract: it decides what a variant *is* here (which GLB),
//! and writes the result as engine-agnostic JSON so the demo can stay headless and still show what
//! a frame would contain.
//!
//! Instances are grouped by model, because that is how a renderer batches them, and groups are
//! emitted in a fixed order so two runs produce byte-identical files.

use crate::assets::{self, ModelKind};
use crate::city::CityLayout;
use crate::json;
use bevy_flock::adapters::primary::render::tint_from_seed;
use bevy_flock::domain::num;
use bevy_math::Vec3;

/// One thing to draw.
#[derive(Debug, Clone)]
pub struct Instance {
    /// Which model, by slug.
    pub model: &'static str,
    /// Where it stands.
    pub position: Vec3,
    /// Facing, radians around up.
    pub yaw: f32,
    /// Uniform scale to apply to the model.
    pub scale: f32,
    /// Base colour multiplied into the model's material.
    pub tint: [f32; 4],
}

/// Everything the scene contains, grouped for batching.
#[derive(Debug, Default)]
pub struct Scene {
    /// Humanoid bodies, one per living NPC the renderer would draw.
    pub bodies: Vec<Instance>,
    /// Wayfinding pylons.
    pub pylons: Vec<Instance>,
    /// Shelter doors.
    pub shelters: Vec<Instance>,
    /// Market stalls.
    pub stalls: Vec<Instance>,
    /// Ruined blocks, drawn from the layout rather than from a model.
    pub blocks: Vec<BlockInstance>,
}

/// A ruined building, which the demo generates instead of generating a mesh for.
///
/// Six bodies and props are worth generating; a hundred building shells are not, and a box with a
/// ruin factor is both cheaper and enough to read as a skyline.
#[derive(Debug, Clone, Copy)]
pub struct BlockInstance {
    /// Centre at ground level.
    pub centre: Vec3,
    /// Half-extents, with the height in `y`.
    pub size: Vec3,
    /// 0 standing, 1 rubble.
    pub ruin: f32,
}

impl Scene {
    /// Build the scene for a city and a set of NPCs.
    ///
    /// `bodies` is `(archetype slug, position, yaw, appearance seed)` per drawn NPC, which is what
    /// a caller can get from `VisibleInstance` plus `NpcIdentity`.
    #[must_use]
    pub fn build(city: &CityLayout, bodies: &[Body]) -> Self {
        let mut scene = Self::default();
        for body in bodies {
            let slug = assets::body_for(body.archetype);
            // Proportional jitter around the shipped height: a crowd of clones reads as a bug, and
            // a body is never scaled so far from its true size that the feet leave the ground.
            let scale = 0.94 + unit(body.appearance_seed) * 0.12;
            scene.bodies.push(Instance {
                model: slug,
                position: body.position,
                yaw: body.yaw,
                scale,
                tint: tint_from_seed(body.appearance_seed),
            });
        }
        // Grouped by model so a renderer gets one batch per mesh. `sort_by_key` is stable, so the
        // order within a model is still the order the caller supplied.
        scene.bodies.sort_by_key(|instance| {
            assets::MODELS
                .iter()
                .position(|model| model.slug == instance.model)
                .unwrap_or(usize::MAX)
        });
        for pylon in &city.pylons {
            scene.pylons.push(Instance {
                model: "pylon",
                position: pylon.position,
                yaw: pylon.yaw,
                scale: 1.0,
                tint: [1.0, 1.0, 1.0, 1.0],
            });
        }
        for shelter in &city.shelters {
            scene.shelters.push(Instance {
                model: "shelter-door",
                position: shelter.position,
                yaw: shelter.yaw,
                scale: 1.0,
                tint: [1.0, 1.0, 1.0, 1.0],
            });
        }
        for stall in &city.stalls {
            scene.stalls.push(Instance {
                model: "market-stall",
                position: stall.position,
                yaw: stall.yaw,
                scale: 1.0,
                tint: [1.0, 1.0, 1.0, 1.0],
            });
        }
        scene.blocks = city
            .blocks
            .iter()
            .map(|block| BlockInstance {
                centre: block.centre,
                size: block.size,
                ruin: block.ruin,
            })
            .collect();
        scene
    }

    /// Every instance, grouped by model in a fixed order.
    #[must_use]
    pub fn groups(&self) -> Vec<(&'static str, &[Instance])> {
        vec![
            ("gatewarden", self.bodies_of("gatewarden")),
            ("aerie", self.bodies_of("aerie")),
            ("scavenger", self.bodies_of("scavenger")),
            ("pylon", &self.pylons),
            ("shelter-door", &self.shelters),
            ("market-stall", &self.stalls),
        ]
    }

    fn bodies_of(&self, slug: &str) -> &[Instance] {
        let mut start = None;
        let mut end = 0;
        for (index, instance) in self.bodies.iter().enumerate() {
            if instance.model == slug {
                if start.is_none() {
                    start = Some(index);
                }
                end = index + 1;
            } else if start.is_some() {
                break;
            }
        }
        start.map_or(&[], |start| self.bodies.get(start..end).unwrap_or(&[]))
    }

    /// How many instances of everything.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bodies.len() + self.pylons.len() + self.shelters.len() + self.stalls.len()
    }

    /// Whether the scene is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Render the scene as JSON.
    #[must_use]
    pub fn to_json(&self, city: &CityLayout, digest: u64) -> String {
        let mut groups = Vec::new();
        for (slug, instances) in self.groups() {
            let Some(model) = assets::model(slug) else {
                continue;
            };
            let items: Vec<String> = instances
                .iter()
                .map(|instance| {
                    json::inline(&[
                        ("position", json::vec3(instance.position)),
                        ("yaw", json::number(instance.yaw, 4)),
                        ("scale", json::number(instance.scale, 3)),
                        (
                            "tint",
                            json::array(&instance.tint.map(|c| json::number(c, 3))),
                        ),
                    ])
                })
                .collect();
            groups.push(json::object(
                &[
                    ("model", json::string(model.file)),
                    ("slug", json::string(slug)),
                    (
                        "kind",
                        json::string(match model.kind {
                            ModelKind::Body => "body",
                            ModelKind::Prop => "prop",
                        }),
                    ),
                    ("count", instances.len().to_string()),
                    ("instances", json::array_block(&items, 4)),
                ],
                2,
            ));
        }
        let blocks: Vec<String> = self
            .blocks
            .iter()
            .map(|block| {
                json::inline(&[
                    ("centre", json::vec3(block.centre)),
                    ("size", json::vec3(block.size)),
                    ("ruin", json::number(block.ruin, 3)),
                ])
            })
            .collect();
        json::object(
            &[
                ("format", json::string("bevy-flock-demo-scene/1")),
                ("seed", city.seed.get().to_string()),
                ("extent", json::number(city.extent, 2)),
                ("digest", format!("\"0x{digest:016x}\"")),
                ("groups", json::array_block(&groups, 0)),
                ("blocks", json::array_block(&blocks, 0)),
            ],
            0,
        )
    }
}

/// A fingerprint of where the crowd actually is.
///
/// The library's `Diagnostics::digest` is deliberately coarse — population, tiers, mood, tick — so
/// that a platform whose transcendentals differ in the last bit does not report a false
/// non-determinism. That is the right digest for a simulation guarantee and the wrong one for an
/// artefact: two cities built from different seeds have the same population and the same mood, so
/// their digests match while their drawings differ completely. This hashes quantised positions to
/// a millimetre instead, which is what "the same run produced the same picture" actually means.
#[must_use]
pub fn crowd_fingerprint(bodies: &[Body]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |value: u64| {
        hash ^= value;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for body in bodies {
        // Quantised, not raw bits: the same reason the library's digest is coarse, applied at the
        // resolution that still distinguishes two different crowds.
        mix(num::i64_to_u64(num::f32_to_i64(
            (body.position.x * 1_000.0).round(),
        )));
        mix(num::i64_to_u64(num::f32_to_i64(
            (body.position.y * 1_000.0).round(),
        )));
        mix(num::i64_to_u64(num::f32_to_i64(
            (body.position.z * 1_000.0).round(),
        )));
        mix(num::i64_to_u64(num::f32_to_i64(
            (body.yaw * 1_000.0).round(),
        )));
        mix(body.appearance_seed);
    }
    hash
}

/// One drawn NPC, as the scene needs it.
#[derive(Debug, Clone, Copy)]
pub struct Body {
    /// Which archetype, which decides the model.
    pub archetype: bevy_flock::domain::temperament::Archetype,
    /// Where it stands.
    pub position: Vec3,
    /// Which way it faces.
    pub yaw: f32,
    /// The seed the library hands a renderer for palette and silhouette.
    pub appearance_seed: u64,
}

/// A unit float from a seed, without a generator: the scene needs one number, not a stream.
fn unit(seed: u64) -> f32 {
    let mixed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    num::u64_to_f32((mixed >> 40) & 0x00FF_FFFF) / 16_777_216.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::domain::ids::SimSeed;
    use bevy_flock::domain::temperament::Archetype;
    use bevy_flock::testkit::OrDie;

    fn city() -> CityLayout {
        CityLayout::generate(SimSeed::new(11), 700.0)
    }

    #[test]
    fn every_shipped_model_appears_in_the_groups() {
        let layout = city();
        let scene = Scene::build(&layout, &[]);
        let slugs: Vec<&str> = scene.groups().iter().map(|(slug, _)| *slug).collect();
        for model in assets::MODELS {
            assert!(slugs.contains(&model.slug), "{} is never drawn", model.slug);
        }
    }

    #[test]
    fn a_body_group_holds_only_its_own_model() {
        let layout = city();
        let bodies = [
            Body {
                archetype: Archetype::Guard,
                position: Vec3::ZERO,
                yaw: 0.0,
                appearance_seed: 1,
            },
            Body {
                archetype: Archetype::Urchin,
                position: Vec3::X,
                yaw: 0.0,
                appearance_seed: 2,
            },
        ];
        let scene = Scene::build(&layout, &bodies);
        let (_, guards) = scene
            .groups()
            .into_iter()
            .find(|(slug, _)| *slug == "gatewarden")
            .or_die();
        assert_eq!(guards.len(), 1);
        assert_eq!(guards.first().map(|i| i.model), Some("gatewarden"));
    }

    #[test]
    fn the_same_scene_renders_the_same_bytes() {
        let layout = city();
        let scene = Scene::build(&layout, &[]);
        let first = scene.to_json(&layout, 0x1234);
        let second = Scene::build(&city(), &[]).to_json(&layout, 0x1234);
        assert_eq!(first, second);
    }

    #[test]
    fn the_document_is_the_format_it_claims() {
        let layout = city();
        let text = Scene::build(&layout, &[]).to_json(&layout, 7);
        assert!(
            text.starts_with("{\n  \"format\": \"bevy-flock-demo-scene/1\""),
            "{text}"
        );
        assert!(
            text.contains("\"digest\": \"0x0000000000000007\""),
            "{text}"
        );
        assert!(text.contains("\"blocks\": ["));
    }
}
