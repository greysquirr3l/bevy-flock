//! Draw the city in three dimensions.
//!
//! The scene is the same `Scene` the 2D artefacts describe — `scene::Scene::build` turns the crowd
//! and the layout into instances, and this module turns instances into triangles. That is the point
//! of the separation: a renderer that walked `CityLayout` itself would be a second description of
//! the city, and the picture would stop being evidence about the scene the JSON describes.
//!
//! Four views, because a demo image has a job to do and one picture cannot do four:
//!
//! - `district` — 90 m over `gate-row`, where the crowd and the buildings are,
//! - `street` — at eye height in the same settlement, where the scale of a person is,
//! - `hero` — 4 m from one scavenger, where the asset's silhouette and detail are,
//! - `skyline` — 700 m up, where the shape of the whole place is.
//!
//! Meshes are loaded once and instanced; a distant crowd is culled by distance and then by count,
//! which is what a real renderer does with an LOD budget and what a CPU rasteriser has to do to
//! finish.

use crate::DemoError;
use crate::assets;
use crate::city::CityLayout;
use crate::gltf::{self, Mesh};
use crate::raster::{Atmosphere, Camera, Face, Image, Renderer, Style, Vertex};
use crate::scene::{Body, Instance, Scene};
use bevy_flock::domain::num;
use bevy_math::{Mat4, Quat, Vec3};
use std::collections::BTreeMap;

/// The meshes the scene can instance, loaded once.
pub struct Meshes {
    by_slug: BTreeMap<&'static str, Mesh>,
}

impl Meshes {
    /// Load every model the asset table promises.
    ///
    /// # Errors
    ///
    /// Propagates [`DemoError::Asset`] from [`gltf::load`], so a corrupt or missing GLB stops the
    /// render instead of drawing part of a city.
    pub fn load() -> Result<Self, DemoError> {
        let mut by_slug = BTreeMap::new();
        for model in &assets::MODELS {
            by_slug.insert(model.slug, gltf::load(&assets::root().join(model.file))?);
        }
        Ok(Self { by_slug })
    }

    /// The mesh for a slug, if it is one of the shipped models.
    #[must_use]
    pub fn get(&self, slug: &str) -> Option<&Mesh> {
        self.by_slug.get(slug)
    }

    /// Total triangles across every model.
    #[must_use]
    pub fn triangles(&self) -> usize {
        self.by_slug.values().map(Mesh::triangles).sum()
    }
}

/// One camera position and what it is for.
#[derive(Debug, Clone)]
pub struct Shot {
    /// File stem: `district`, `street`, `hero`, `skyline`.
    pub name: &'static str,
    /// What the image shows, for the report and the README.
    pub note: &'static str,
    /// The camera.
    pub camera: Camera,
    /// Lighting and fog for this view.
    pub atmosphere: Atmosphere,
    /// Bodies within this radius of the eye are drawn, metres.
    pub draw_distance: f32,
    /// No more than this many bodies, nearest first.
    pub max_bodies: usize,
    /// Bodies nearer the eye than this are skipped, metres: the camera should not be inside
    /// somebody's shoulder.
    pub body_margin: f32,
    /// Ruins within this radius, metres.
    pub block_distance: f32,
    /// How far the ground plane reaches from the origin, metres.
    pub ground_reach: f32,
    /// Ground tile size, metres.
    pub ground_tile: f32,
    /// Smallest triangle worth shading, pixels; zero draws every triangle.
    pub min_area: f32,
    /// Beyond this distance a body is drawn as a two-triangle billboard, metres.
    ///
    /// Infinite by default, which draws every triangle of every body: the shipped PNGs are made at
    /// full detail because they are read, not steered. A forty-metre billboard covers a handful of
    /// pixels in a terminal, where the mesh beside it costs eleven thousand triangles for the same
    /// handful.
    pub impostor_distance: f32,
    /// Width of the cross-fade below [`Self::impostor_distance`], metres; zero for a hard switch.
    ///
    /// A body between `impostor_distance - impostor_band` and `impostor_distance` is drawn twice: its
    /// mesh, and its billboard composited on top with alpha ramping from nothing to opaque. Without
    /// the band a body walking across the threshold pops between eleven thousand triangles and two,
    /// which is invisible at a terminal's three pixels a body and obvious on a larger one. A band is
    /// not free — the bodies inside it pay for both representations — so it is zero on the five
    /// shipped views and eight metres wide on the interactive budget.
    pub impostor_band: f32,
}

const fn camera(eye: Vec3, target: Vec3, fov_y: f32) -> Camera {
    Camera {
        eye,
        target,
        fov_y,
        near: 0.08,
        far: 4000.0,
    }
}

/// The four views, framed from the layout rather than from hard-coded coordinates, so a different
/// seed still frames a settlement instead of empty ground.
#[must_use]
/// Everything about this crowd that the shot table needs before it can place a camera.
struct Framing {
    /// The settlement every view is framed on.
    centre: Vec3,
    /// Where the street camera stands and looks.
    street_eye: Vec3,
    /// The portrait's camera.
    hero_eye: Vec3,
    hero_target: Vec3,
    /// The four lighting rigs.
    district: Atmosphere,
    street_air: Atmosphere,
    hero_air: Atmosphere,
    skyline: Atmosphere,
}

/// Pick the subject, the vantage and the light. Split out of [`shots`] so the shot table stays a
/// table rather than a page of arithmetic.
fn framing(city: &CityLayout, bodies: &[Body]) -> Framing {
    let centre = settlement_centre(city);

    // A street eye and a hero subject are both facts about the crowd, not about the layout: the
    // camera should stand in a gap and the close-up should be of somebody who exists. Picking both
    // from the scene is what keeps a different seed framing a settlement instead of a void.
    // The subject is the body with the most room around it: a portrait of somebody standing in a
    // crowd is a portrait of somebody's shoulder.
    let hero = (0..bodies.len())
        .map(|index| {
            let body = bodies.get(index);
            let position = body.map_or(centre, |body| body.position);
            let clearance = bodies
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| other.position.distance(position))
                .fold(f32::INFINITY, f32::min);
            let clearance = if clearance.is_finite() {
                clearance
            } else {
                f32::MAX
            };
            // Standing in the open means clear of the ruins as well as of other people.
            let walls = city
                .blocks
                .iter()
                .map(|block| {
                    let extent = block.size.x.max(block.size.z) * 0.5;
                    block.centre.distance(position) - extent
                })
                .fold(f32::INFINITY, f32::min);
            let walls = if walls.is_finite() { walls } else { f32::MAX };
            (index, clearance.min(walls))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .and_then(|(index, _)| bodies.get(index))
        .map(|body| (body.position, body.yaw));
    let (hero_position, hero_yaw) = hero.unwrap_or((centre, 0.0));

    let street = street_vantage(centre, bodies);

    let mut district = Atmosphere {
        fog_near: 200.0,
        fog_far: 1400.0,
        ..Atmosphere::default()
    };
    district.sun = Vec3::new(-0.42, 0.72, 0.55).normalize();
    district.fog = [0.70, 0.66, 0.62];

    let mut street_air = Atmosphere {
        fog_near: 70.0,
        fog_far: 260.0,
        ..Atmosphere::default()
    };
    street_air.sun = Vec3::new(-0.58, 0.52, 0.63).normalize();
    street_air.fog = [0.66, 0.63, 0.59];

    let hero_air = Atmosphere {
        fog_near: 30.0,
        fog_far: 220.0,
        fog: [0.62, 0.60, 0.57],
        ..Atmosphere::default()
    };

    // Fog closes before the ground plane ends: at 1500 m of ground and 4200 m of fog the far edge
    // of the plane stayed visible as a pale ridge above the horizon, which read as a hill that the
    // layout does not have.
    // Aerial perspective over about a kilometre: close enough that the near ruins read, far enough
    // that the plane's own edge is fully fogged before it is reached.
    let mut skyline = Atmosphere {
        fog_near: 380.0,
        fog_far: 1980.0,
        fog: [0.70, 0.66, 0.62],
        ..Atmosphere::default()
    };
    skyline.sun = Vec3::new(-0.38, 0.62, 0.68).normalize();

    // The hero camera stands in front of the subject and slightly to its left, at chest height,
    // looking at the chest: a portrait, not an orbit.
    // Face the subject and stand on whichever of its two front quarters catches the sun: a
    // portrait of a silhouette is a silhouette, not a portrait. The model's front is `+Z` in its
    // own frame, which is the convention `gltf` bakes and the one the crowd's yaw is measured in.
    let front = Vec3::new(hero_yaw.sin(), 0.0, hero_yaw.cos());
    let toward_sun = Vec3::new(hero_air.sun.x, 0.0, hero_air.sun.z).normalize_or_zero();
    let forward = if front.dot(toward_sun) < 0.0 {
        -front
    } else {
        front
    };
    let side = Vec3::new(forward.z, 0.0, -forward.x);
    let hero_eye = hero_position + forward * 5.0 + side * 0.9 + Vec3::new(0.0, 1.5, 0.0);
    let hero_target = hero_position + Vec3::new(0.0, 1.06, 0.0);

    Framing {
        centre,
        street_eye: street,
        hero_eye,
        hero_target,
        district,
        street_air,
        hero_air,
        skyline,
    }
}

/// The five views: frame, light and budget for each.
#[must_use]
pub fn shots(city: &CityLayout, bodies: &[Body]) -> Vec<Shot> {
    let Framing {
        centre,
        street_eye: street,
        hero_eye,
        hero_target,
        district,
        street_air,
        hero_air,
        skyline,
    } = framing(city, bodies);
    vec![
        Shot {
            name: "district",
            note: "gate-row from 48 m: the crowd, the stalls, the ruins between them",
            camera: camera(
                centre + Vec3::new(-58.0, 46.0, 62.0),
                centre + Vec3::new(0.0, 2.0, 0.0),
                0.95,
            ),
            atmosphere: district,
            body_margin: 0.0,
            draw_distance: 190.0,
            max_bodies: 420,
            block_distance: 380.0,
            min_area: 0.0,
            impostor_distance: f32::INFINITY,
            impostor_band: 0.0,
            ground_reach: 700.0,
            ground_tile: 40.0,
        },
        Shot {
            name: "street",
            note: "the same settlement at eye height, where a 1.8 m person is 1.8 m",
            camera: camera(street, street - Vec3::new(0.0, 0.06, 46.0), 1.02),
            atmosphere: street_air,
            body_margin: 1.2,
            draw_distance: 140.0,
            max_bodies: 200,
            block_distance: 220.0,
            min_area: 0.0,
            impostor_distance: f32::INFINITY,
            impostor_band: 0.0,
            ground_reach: 420.0,
            ground_tile: 24.0,
        },
        Shot {
            name: "procession",
            note: "walkers on the settling's main street, one rig, fourteen phases",
            camera: camera(
                Vec3::new(centre.x - 1.6, 1.62, centre.z + 8.0),
                Vec3::new(centre.x + 0.2, 1.02, centre.z - 12.0),
                0.66,
            ),
            atmosphere: Atmosphere {
                fog_near: 90.0,
                fog_far: 520.0,
                ..Atmosphere::default()
            },
            draw_distance: 120.0,
            max_bodies: 10,
            body_margin: 1.4,
            block_distance: 150.0,
            min_area: 0.0,
            impostor_distance: f32::INFINITY,
            impostor_band: 0.0,
            ground_reach: 220.0,
            ground_tile: 12.0,
        },
        Shot {
            name: "hero",
            note: "one scavenger at 4.4 m: the shipped mesh, not the concept",
            camera: camera(hero_eye, hero_target, 0.68),
            atmosphere: hero_air,
            body_margin: 2.8,
            draw_distance: 40.0,
            max_bodies: 4,
            block_distance: 80.0,
            min_area: 0.0,
            impostor_distance: f32::INFINITY,
            impostor_band: 0.0,
            ground_reach: 120.0,
            ground_tile: 12.0,
        },
        skyline_shot(centre, skyline),
    ]
}

/// The far view: the whole verge, from high enough that six settlements fit in the frame.
fn skyline_shot(centre: Vec3, atmosphere: Atmosphere) -> Shot {
    Shot {
        name: "skyline",
        note: "the whole verge from 620 m: six settlements and the ruins between them",
        camera: camera(
            centre + Vec3::new(-330.0, 208.0, 452.0),
            centre + Vec3::new(-70.0, 6.0, -170.0),
            0.72,
        ),
        atmosphere,
        body_margin: 0.0,
        draw_distance: 1400.0,
        max_bodies: 900,
        block_distance: 2100.0,
        min_area: 0.0,
        impostor_distance: f32::INFINITY,
        impostor_band: 0.0,
        ground_reach: 2200.0,
        ground_tile: 110.0,
    }
}

/// A place to stand in the street: 34 m south of the settlement centre, nudged along the street
/// until nothing is standing in the camera's lap.
fn street_vantage(centre: Vec3, bodies: &[Body]) -> Vec3 {
    let base = centre + Vec3::new(0.0, 1.68, 34.0);
    let mut best = base;
    let mut best_gap = f32::NEG_INFINITY;
    for step in -6..=6 {
        let candidate = base + Vec3::new(num::i32_to_f32(step) * 2.5, 0.0, 0.0);
        let gap = bodies
            .iter()
            .map(|body| body.position.distance(candidate))
            .fold(f32::INFINITY, f32::min);
        if gap > best_gap {
            best_gap = gap;
            best = candidate;
        }
    }
    best
}

/// What one render drew, for the report.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Bodies drawn.
    pub bodies: usize,
    /// Props drawn (pylons, shelters, stalls).
    pub props: usize,
    /// Ruined blocks drawn.
    pub blocks: usize,
    /// Triangles submitted to the rasteriser.
    pub triangles: usize,
    /// Fraction of pixels that are not sky.
    pub coverage: f32,
    /// Wall time, milliseconds.
    pub ms: f64,
    /// Fingerprint of the pixels.
    pub fingerprint: u64,
}

/// The settlement the views are framed on: the largest guild town, or the first settlement.
///
/// Shared by the shot table and the procession, because framing the camera on one settlement and
/// marching the column through another puts fourteen animated walkers outside the frustum — which
/// is exactly how the first cut of the procession rendered an empty street.
#[must_use]
pub fn settlement_centre(city: &CityLayout) -> Vec3 {
    city.districts
        .iter()
        .find(|district| district.guild && district.population_share > 0.3)
        .or_else(|| city.districts.first())
        .map_or(Vec3::ZERO, |district| district.centre)
}

/// One animated walker: where it stands, which way it faces, and where in its clip it is.
#[derive(Debug, Clone, Copy)]
pub struct Walker {
    /// Feet position, metres.
    pub position: Vec3,
    /// Facing, radians about `Y`.
    pub yaw: f32,
    /// How tall this one stands, metres.
    pub height: f32,
    /// Multiplied into the rig's albedo.
    pub tint: [f32; 3],
    /// Offset into the clip, seconds.
    pub phase: f32,
}

/// A column of walkers on a street, driving one clip from staggered phases.
///
/// The phases are staggered by the golden ratio rather than evenly spaced: an even spacing makes a
/// column march in lockstep, which reads as a machine rather than as a crowd.
#[must_use]
pub fn procession(
    centre: Vec3,
    rig: &crate::skinned::Rig,
    clip: &str,
    count: usize,
) -> Vec<Walker> {
    let duration = rig
        .clips()
        .get(clip)
        .map_or(1.0, crate::skinned::Clip::duration);
    // Just off the settlement's centre line. `block_pitch` is the *ruin* pitch, tens of metres,
    // and using a multiple of it put the column a hundred metres off the camera's axis: fourteen
    // animated walkers, correctly skinned, entirely outside the frustum.
    let lane_x = centre.x;
    (0..count)
        .map(|index| {
            let step = f32::from(u16::try_from(index).unwrap_or(0));
            Walker {
                position: Vec3::new(
                    lane_x + (step * 2.399_963).sin() * 0.45,
                    0.0,
                    centre.z - 30.0 + step * 1.45,
                ),
                // Facing `+Z`, which is the direction the column walks and the direction the
                // procession camera looks from: the crowd comes towards the lens.
                yaw: 0.0,
                height: 1.62 + (step * 1.7).sin() * 0.09,
                tint: [
                    0.86 + (step * 2.1).sin() * 0.22,
                    0.80 + (step * 1.3).cos() * 0.20,
                    0.74 + (step * 0.7).sin() * 0.24,
                ],
                phase: (step * 0.618_034).fract() * duration,
            }
        })
        .collect()
}

/// Render one view.
#[must_use]
pub fn render(
    scene: &Scene,
    meshes: &Meshes,
    shot: &Shot,
    width: u32,
    height: u32,
) -> (Image, Stats) {
    render_with(scene, meshes, &[], shot, width, height)
}

/// Render one view, drawing a procession of animated walkers alongside the static bodies.
///
/// Each entry is a rig, the clip to play, and the walkers driving it. Skinning happens per
/// instance on the CPU: a rig of twenty-three joints and two thousand triangles is far cheaper to
/// evaluate than to upload, and it keeps the demo free of a GPU dependency.
/// Draw the ground, the ruins, the props and the static bodies, in that order.
///
/// Split out of [`render_with`] so the walker pass reads as its own stage: the static world is one
/// thing, a crowd of skinned instances another, and the frame is the sum.
fn draw_statics(
    renderer: &Renderer,
    image: &mut Image,
    scene: &Scene,
    meshes: &Meshes,
    shot: &Shot,
) -> Stats {
    let mut stats = Stats::default();

    // The ground first: it fills the lower half of every view and is the cheapest thing to draw.
    push_ground(
        renderer,
        image,
        shot.camera.eye,
        shot.ground_reach,
        shot.ground_tile,
    );

    for block in &scene.blocks {
        if block.centre.distance(shot.camera.eye) > shot.block_distance {
            continue;
        }
        let albedo = block_albedo(block.ruin);
        push_box(
            renderer,
            image,
            block.centre,
            block.size,
            albedo,
            Style::Facade { storey: 3.4 },
        );
        stats.blocks += 1;
        stats.triangles += 12;
    }

    for group in [&scene.pylons, &scene.shelters, &scene.stalls] {
        for instance in group {
            if instance.position.distance(shot.camera.eye) > shot.block_distance {
                continue;
            }
            if let Some(mesh) = meshes.get(instance.model) {
                stats.triangles += push_instance(renderer, image, mesh, instance);
                stats.props += 1;
            }
        }
    }

    // Bodies: nearest first, then cut at the budget. A renderer with an LOD budget does the same,
    // and a crowd four hundred metres away in a 1200-pixel image is a smear either way.
    let mut nearby: Vec<(&Instance, f32)> = scene
        .bodies
        .iter()
        .map(|instance| (instance, instance.position.distance(shot.camera.eye)))
        .filter(|(_, distance)| *distance <= shot.draw_distance)
        .collect();
    nearby.sort_by(|left, right| left.1.total_cmp(&right.1));
    let band = shot.impostor_band.max(0.0);
    let fade_start = shot.impostor_distance - band;
    for (instance, distance) in nearby.into_iter().take(shot.max_bodies) {
        stats.bodies += 1;
        let mesh = meshes.get(instance.model);
        let base = mesh.map_or([0.5, 0.5, 0.5], |mesh| mesh.base_colour);
        if distance > shot.impostor_distance {
            push_billboard(renderer, image, &shot.camera, instance, base, 1.0);
            stats.triangles += 2;
        } else if band > 0.0 && distance > fade_start {
            // Cross-fade by coverage: the quad claims an increasing share of the pixels first, then
            // the mesh gives its own up, so a body dissolves instead of popping. Both passes stay
            // opaque, so they run in the ordinary near-to-far order with no blending and no depth
            // tricks, and every pixel belongs to exactly one of the two.
            let coverage = ((distance - fade_start) / band).clamp(0.0, 1.0);
            push_billboard(renderer, image, &shot.camera, instance, base, coverage);
            stats.triangles += 2;
            if let Some(mesh) = mesh {
                stats.triangles +=
                    push_instance_coverage(renderer, image, mesh, instance, 1.0 - coverage);
            }
        } else if let Some(mesh) = mesh {
            stats.triangles += push_instance(renderer, image, mesh, instance);
        }
    }

    stats
}

/// Draw a body as a camera-facing quad, which is what a body looks like at forty metres.
///
/// Two triangles, a body's height and a third of it wide, tinted with the instance's own colour and
/// with normals pointing back at the camera so the sun shades it the way the mesh would. The mesh is
/// eleven thousand triangles; at forty metres it covers a dozen pixels, which is what this is for.
fn push_billboard(
    renderer: &Renderer,
    image: &mut Image,
    camera: &Camera,
    instance: &Instance,
    base: [f32; 3],
    coverage: f32,
) {
    let forward = (camera.target - camera.eye).normalize_or_zero();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    let height = 1.75 * instance.scale;
    // Shoulder width is about a quarter of a standing height.
    let half_width = height * 0.13;
    // Five centimetres toward the eye. A body's mesh is a surface with volume and the quad stands in
    // the middle of it, so without the nudge the quad loses the depth test against the body it is
    // meant to cover and the fade is invisible; five centimetres is a fifth of a pixel of parallax at
    // the twenty-four metres where the switch lives.
    let foot = instance.position - forward * 0.05;
    let bottom_left = foot - right * half_width;
    let bottom_right = foot + right * half_width;
    let top_left = bottom_left + up * height;
    let top_right = bottom_right + up * height;
    let normal = -forward;
    // The material the mesh beside it would have used: the asset table's colour times the instance
    // tint, so a billboard and a body at the same distance agree.
    let material = assets::model(instance.model).map_or(base, |model| model.material);
    let tint = [
        material[0] * instance.tint[0],
        material[1] * instance.tint[1],
        material[2] * instance.tint[2],
    ];
    let vertex = |world: Vec3| Vertex { world, normal };
    for triangle in [
        [bottom_left, bottom_right, top_right],
        [bottom_left, top_right, top_left],
    ] {
        renderer.triangle_coverage(
            image,
            &Face {
                vertices: triangle.map(vertex),
                albedo: tint,
                style: Style::Lit,
            },
            coverage,
        );
    }
}

/// Render a shot, drawing a procession of walkers as well as the static bodies.
///
/// Each entry is a rig, the clip to play, and the walkers driving it. Skinning happens per instance
/// on the CPU: a rig of twenty-three joints and a few thousand triangles is cheaper to evaluate than
/// to upload, and it keeps the demo free of a GPU dependency.
pub fn render_with(
    scene: &Scene,
    meshes: &Meshes,
    walkers: &[(&crate::skinned::Rig, &str, &[Walker])],
    shot: &Shot,
    width: u32,
    height: u32,
) -> (Image, Stats) {
    let started = std::time::Instant::now();
    let aspect = num::u32_to_f32(width) / num::u32_to_f32(height.max(1));
    let renderer =
        Renderer::new(&shot.camera, aspect, shot.atmosphere).with_min_area(shot.min_area);
    let mut image = Image::new(width, height);
    // Where the camera's eye level cuts the frame, in NDC then in rows: the sky gradient reaches
    // the fog colour exactly there, so the ground plane's far edge has nothing to end against.
    let forward = (shot.camera.target - shot.camera.eye).normalize_or_zero();
    let pitch = forward.y.clamp(-1.0, 1.0).asin();
    let horizon_ndc = -pitch.tan() / (shot.camera.fov_y * 0.5).tan().max(1.0e-4);
    let rows = f32::from(u16::try_from(height).unwrap_or(1));
    let horizon_row = num::f32_to_u32(
        ((1.0 - (horizon_ndc * 0.5 + 0.5)) * rows)
            .clamp(0.0, rows)
            .round(),
    );
    image.sky_to(
        [
            shot.atmosphere.sky[0] * 0.55 + 0.05,
            shot.atmosphere.sky[1] * 0.55 + 0.07,
            shot.atmosphere.sky[2] * 0.55 + 0.12,
        ],
        shot.atmosphere.fog,
        horizon_row,
    );

    let mut stats = draw_statics(&renderer, &mut image, scene, meshes, shot);

    for (rig, clip, column) in walkers {
        let Some(clip) = rig.clips().get(*clip) else {
            continue;
        };
        let scale = if rig.height() > 0.0 {
            1.0 / rig.height()
        } else {
            1.0
        };
        for walker in *column {
            if walker.position.distance(shot.camera.eye) > shot.draw_distance {
                continue;
            }
            let pose = rig.pose(clip, walker.phase);
            let transform = Mat4::from_scale_rotation_translation(
                Vec3::splat(walker.height * scale),
                Quat::from_rotation_y(walker.yaw),
                walker.position,
            );
            let albedo = [
                0.44 * walker.tint[0],
                0.41 * walker.tint[1],
                0.38 * walker.tint[2],
            ];
            let mut triangles = 0_usize;
            rig.skin(&pose, transform, |positions, normals| {
                renderer.triangle(
                    &mut image,
                    &Face {
                        vertices: [
                            Vertex {
                                world: positions[0],
                                normal: normals[0],
                            },
                            Vertex {
                                world: positions[1],
                                normal: normals[1],
                            },
                            Vertex {
                                world: positions[2],
                                normal: normals[2],
                            },
                        ],
                        albedo,
                        style: Style::Lit,
                    },
                );
                triangles += 1;
            });
            stats.triangles += triangles;
            stats.bodies += 1;
        }
    }

    stats.coverage = image.coverage();
    stats.ms = started.elapsed().as_secs_f64() * 1000.0;
    stats.fingerprint = image.fingerprint();
    (image, stats)
}

fn block_albedo(ruin: f32) -> [f32; 3] {
    let intact = [0.33, 0.30, 0.27];
    let dusty = [0.21, 0.19, 0.17];
    [
        intact[0] + (dusty[0] - intact[0]) * ruin,
        intact[1] + (dusty[1] - intact[1]) * ruin,
        intact[2] + (dusty[2] - intact[2]) * ruin,
    ]
}

fn push_ground(renderer: &Renderer, image: &mut Image, eye: Vec3, reach: f32, tile: f32) {
    // Tiles rather than one enormous quad: a single 6 km triangle is numerically hostile to a
    // perspective divide and gets culled wholesale when any corner falls behind the near plane,
    // which is exactly what the first render of this scene showed — half a city on bare sky.
    // Centred on the eye, snapped to the tile pitch so the pattern does not swim between frames.
    // Centring on the world origin instead is what left the hero view standing on a patch of
    // nothing: the settlement is 60 m from the origin and the ground only reached half of that.
    let anchor = Vec3::new(
        (eye.x / tile).round() * tile,
        0.0,
        (eye.z / tile).round() * tile,
    );
    let tiles = num::f32_to_i32(((reach * 2.0) / tile).ceil()) + 2;
    let half = tiles / 2;
    for row in -half..half {
        for column in -half..half {
            let x = anchor.x + num::i32_to_f32(column) * tile;
            let z = anchor.z + num::i32_to_f32(row) * tile;
            let corners = [
                Vec3::new(x, 0.0, z),
                Vec3::new(x + tile, 0.0, z),
                Vec3::new(x + tile, 0.0, z + tile),
                Vec3::new(x, 0.0, z + tile),
            ];
            let albedo = [0.185, 0.170, 0.156];
            for triangle in [
                [corners[0], corners[1], corners[2]],
                [corners[0], corners[2], corners[3]],
            ] {
                renderer.triangle(
                    image,
                    &Face {
                        vertices: triangle.map(|world| Vertex {
                            world,
                            normal: Vec3::Y,
                        }),
                        albedo,
                        style: Style::Ground { cell: 9.0 },
                    },
                );
            }
        }
    }
}

/// Push one instanced mesh, returning the triangles submitted.
fn push_instance(
    renderer: &Renderer,
    image: &mut Image,
    mesh: &Mesh,
    instance: &Instance,
) -> usize {
    push_instance_coverage(renderer, image, mesh, instance, 1.0)
}

/// Push one instanced mesh at `coverage`, for the cross-fade; one is [`push_instance`].
fn push_instance_coverage(
    renderer: &Renderer,
    image: &mut Image,
    mesh: &Mesh,
    instance: &Instance,
    coverage: f32,
) -> usize {
    let (sin, cos) = instance.yaw.sin_cos();
    let rotation = |vector: Vec3| -> Vec3 {
        Vec3::new(
            vector.x * cos + vector.z * sin,
            vector.y,
            -vector.x * sin + vector.z * cos,
        )
    };
    // The asset table's material, not the glTF base colour: these are texture-only materials whose
    // factor is white, and the rasteriser does not sample textures.
    let material = assets::model(instance.model).map_or(mesh.base_colour, |model| model.material);
    let albedo = [
        material[0] * instance.tint[0],
        material[1] * instance.tint[1],
        material[2] * instance.tint[2],
    ];
    let world = |index: usize| -> Option<Vertex> {
        let position = mesh.positions.get(index)?;
        let normal = mesh.normals.get(index).copied().unwrap_or([0.0, 1.0, 0.0]);
        let scaled = Vec3::from_array(*position) * instance.scale;
        Some(Vertex {
            world: instance.position + rotation(scaled),
            normal: rotation(Vec3::from_array(normal)),
        })
    };
    let mut submitted = 0;
    for triangle in mesh.indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(c)) = (
            world(triangle.first().copied().unwrap_or(0) as usize),
            world(triangle.get(1).copied().unwrap_or(0) as usize),
            world(triangle.get(2).copied().unwrap_or(0) as usize),
        ) else {
            continue;
        };
        renderer.triangle_coverage(
            image,
            &Face {
                vertices: [a, b, c],
                albedo,
                style: Style::Lit,
            },
            coverage,
        );
        submitted += 1;
    }
    submitted
}

/// Push an axis-aligned box as twelve triangles.
fn push_box(
    renderer: &Renderer,
    image: &mut Image,
    centre: Vec3,
    size: Vec3,
    albedo: [f32; 3],
    style: Style,
) {
    let half = size * 0.5;
    let corner =
        |x: f32, y: f32, z: f32| -> Vec3 { centre + Vec3::new(x * half.x, y * half.y, z * half.z) };
    let faces: [(Vec3, [(f32, f32, f32); 4]); 5] = [
        // top
        (
            Vec3::Y,
            [
                (-1.0, 1.0, -1.0),
                (-1.0, 1.0, 1.0),
                (1.0, 1.0, 1.0),
                (1.0, 1.0, -1.0),
            ],
        ),
        // north (-z)
        (
            Vec3::NEG_Z,
            [
                (-1.0, -1.0, -1.0),
                (1.0, -1.0, -1.0),
                (1.0, 1.0, -1.0),
                (-1.0, 1.0, -1.0),
            ],
        ),
        // south (+z)
        (
            Vec3::Z,
            [
                (-1.0, -1.0, 1.0),
                (-1.0, 1.0, 1.0),
                (1.0, 1.0, 1.0),
                (1.0, -1.0, 1.0),
            ],
        ),
        // west (-x)
        (
            Vec3::NEG_X,
            [
                (-1.0, -1.0, -1.0),
                (-1.0, 1.0, -1.0),
                (-1.0, 1.0, 1.0),
                (-1.0, -1.0, 1.0),
            ],
        ),
        // east (+x)
        (
            Vec3::X,
            [
                (1.0, -1.0, -1.0),
                (1.0, -1.0, 1.0),
                (1.0, 1.0, 1.0),
                (1.0, 1.0, -1.0),
            ],
        ),
    ];
    for (normal, quad) in faces {
        let vertices = quad.map(|(x, y, z)| Vertex {
            world: corner(x, y, z),
            normal,
        });
        for triangle in [
            [vertices[0], vertices[1], vertices[2]],
            [vertices[0], vertices[2], vertices[3]],
        ] {
            renderer.triangle(
                image,
                &Face {
                    vertices: triangle,
                    albedo,
                    style,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::domain::ids::SimSeed;
    use bevy_flock::testkit::OrDie;

    fn sample() -> (CityLayout, Scene, Vec<Body>) {
        let city = CityLayout::generate(SimSeed::new(7), 900.0);
        let centre = city
            .districts
            .iter()
            .map(|district| district.centre)
            .next()
            .unwrap_or(Vec3::ZERO);
        let bodies: Vec<Body> = (0..24_u64)
            .map(|index| Body {
                archetype: bevy_flock::domain::temperament::Archetype::Urchin,
                position: centre + Vec3::new(num::u64_to_f32(index % 6) * 3.0 - 9.0, 0.0, -8.0),
                yaw: 0.0,
                appearance_seed: index,
            })
            .collect();
        let scene = Scene::build(&city, &bodies);
        (city, scene, bodies)
    }

    #[test]
    fn every_model_in_the_scene_has_a_mesh() {
        let (city, _scene, bodies) = sample();
        let meshes = Meshes::load().or_die();
        for model in &assets::MODELS {
            assert!(
                meshes.get(model.slug).is_some(),
                "{} has no mesh",
                model.slug
            );
        }
        assert!(
            meshes.triangles() > 40_000,
            "the meshes are suspiciously small"
        );
        assert!(!city.districts.is_empty());
        assert!(!bodies.is_empty());
    }

    #[test]
    fn shots_are_framed_on_a_settlement_and_look_forward() {
        let (city, _scene, bodies) = sample();
        let shots = shots(&city, &bodies);
        assert_eq!(shots.len(), 5);
        for shot in &shots {
            // A portrait stands closer than a street view, so the floor is a metre rather than a
            // hundred: what matters is that the target is not the eye.
            assert!(
                shot.camera.target.distance(shot.camera.eye) > 1.0,
                "{} has no distance between eye and target",
                shot.name
            );
            assert!(shot.camera.eye.y > 0.0, "{} is underground", shot.name);
            assert!(shot.fov_y_is_sane(), "{}", shot.name);
        }
        let Some(street) = shots.iter().find(|shot| shot.name == "street") else {
            return;
        };
        assert!(
            (street.camera.eye.y - 1.68).abs() < 0.01,
            "the street shot is not at eye height"
        );
    }

    impl Shot {
        fn fov_y_is_sane(&self) -> bool {
            self.camera.fov_y > 0.2 && self.camera.fov_y < 1.8
        }
    }

    #[test]
    fn a_render_draws_the_city_and_repeats_exactly() {
        let (city, scene, bodies) = sample();
        let meshes = Meshes::load().or_die();
        let shot = shots(&city, &bodies)
            .into_iter()
            .find(|shot| shot.name == "district")
            .or_die();

        let (first, first_stats) = render(&scene, &meshes, &shot, 320, 200);
        assert!(first_stats.blocks > 10, "no ruins in the district view");
        assert!(first_stats.triangles > 1000, "nothing was submitted");
        assert!(
            first_stats.coverage > 0.25,
            "the district view is mostly sky: {:.2}",
            first_stats.coverage
        );

        let (second, second_stats) = render(&scene, &meshes, &shot, 320, 200);
        assert_eq!(
            first.fingerprint(),
            second.fingerprint(),
            "two renders differ"
        );
        assert_eq!(first_stats.triangles, second_stats.triangles);
        assert_eq!(first_stats.bodies, second_stats.bodies);
    }

    /// A camera at eye height on the axis of a settlement, with `count` bodies ahead of it.
    ///
    /// The shot has no ruins, no props and a two-tile patch of ground, so `stats.triangles` counts
    /// bodies and nothing else, and a body's distance from the eye is `sqrt(z^2 + 1.7^2)`.
    fn avenue(count: usize, first: f32, step: f32) -> (CityLayout, Scene, Meshes, Shot) {
        let city = CityLayout::generate(SimSeed::new(7), 900.0);
        let centre = city
            .districts
            .iter()
            .map(|district| district.centre)
            .next()
            .unwrap_or(Vec3::ZERO);
        let bodies: Vec<crate::scene::Body> = (0..count as u64)
            .map(|index| crate::scene::Body {
                archetype: bevy_flock::domain::temperament::Archetype::Urchin,
                position: centre + Vec3::new(0.0, 0.0, -(first + num::u64_to_f32(index) * step)),
                yaw: 0.0,
                appearance_seed: index,
            })
            .collect();
        let mut shot = shots(&city, &bodies).into_iter().next().or_die();
        shot.camera = Camera {
            eye: centre + Vec3::new(0.0, 1.7, 0.0),
            target: centre + Vec3::new(0.0, 1.7, -80.0),
            fov_y: 0.9,
            ..shot.camera
        };
        shot.max_bodies = 64;
        shot.draw_distance = 400.0;
        shot.block_distance = 0.0;
        shot.body_margin = 0.0;
        shot.min_area = 0.0;
        shot.impostor_distance = f32::INFINITY;
        let scene = Scene::build(&city, &bodies);
        let meshes = Meshes::load().or_die();
        (city, scene, meshes, shot)
    }

    #[test]
    fn the_impostor_threshold_decides_between_mesh_and_billboard() {
        let (_, scene, meshes, shot) = avenue(8, 6.0, 6.0);
        let trace = |threshold: f32| {
            let mut shot = shot.clone();
            shot.impostor_distance = threshold;
            let (_, stats) = render(&scene, &meshes, &shot, 200, 140);
            (stats.bodies, stats.triangles)
        };

        // Every body at full detail: the eight meshes, none of them culled.
        let (bodies, detailed) = trace(f32::INFINITY);
        assert_eq!(bodies, 8, "the avenue lost a body");
        // Every body a billboard: exactly two triangles each, whatever the mesh costs.
        let (_, billboards) = trace(0.0);
        assert_eq!(
            billboards,
            2 * bodies,
            "a billboard is not two triangles: {billboards} for {bodies} bodies"
        );
        assert!(
            detailed > billboards + 1000,
            "the meshes cost only {} triangles more than the quads",
            detailed - billboards
        );

        // The switch moves with the threshold rather than sitting at a fixed distance: bodies sit at
        // 6.24, 12.1, 18.1, 24.1 ... metres from an eye 1.7 m up, so a threshold inside the gap draws
        // one more mesh than one inside the gap below it.
        // Every body is the same archetype, so one mesh's cost is the average, and the integer
        // division below counts bodies rather than fractions of one.
        let per_mesh = (detailed - billboards) / bodies;
        assert!(per_mesh > 100, "a mesh costs {per_mesh} triangles");
        let count_meshes = |threshold: f32| {
            let (_, triangles) = trace(threshold);
            (triangles - billboards) / per_mesh
        };
        assert_eq!(count_meshes(5.0), 0, "a 5 m threshold drew a mesh");
        assert_eq!(
            count_meshes(7.0),
            1,
            "a 7 m threshold missed the 6.24 m body"
        );
        assert_eq!(
            count_meshes(13.0),
            2,
            "a 13 m threshold missed the 12.1 m body"
        );
        assert_eq!(
            count_meshes(19.0),
            3,
            "a 19 m threshold missed the 18.1 m body"
        );
        // Monotone: a threshold can only ever draw fewer meshes.
        let mut previous = 0;
        for step in 1..=50 {
            let drawn = count_meshes(num::i32_to_f32(step));
            assert!(drawn >= previous, "the switch went backwards at {step} m");
            previous = drawn;
        }
        assert_eq!(previous, 8, "the furthest body is 48 m away");
    }

    /// Where a world point lands in pixels, straight through the camera's own matrix.
    fn project(camera: &Camera, width: u32, height: u32, world: Vec3) -> (f32, f32) {
        let aspect = num::u32_to_f32(width) / num::u32_to_f32(height);
        let clip = camera.view_projection(aspect) * world.extend(1.0);
        let ndc_x = clip.x / clip.w;
        let ndc_y = clip.y / clip.w;
        (
            (ndc_x * 0.5 + 0.5) * num::u32_to_f32(width),
            (0.5 - ndc_y * 0.5) * num::u32_to_f32(height),
        )
    }

    #[test]
    fn a_billboard_stands_on_the_body_and_faces_the_camera() {
        // One body twenty metres dead ahead, and a reference frame with no bodies at all: the pixels
        // that differ are the billboard, with the ground and the sky cancelled out.
        let (_, scene, meshes, shot) = avenue(1, 20.0, 1.0);
        let width = 160;
        let height = 160;
        let mut empty = shot.clone();
        empty.max_bodies = 0;
        let (reference, _) = render(&scene, &meshes, &empty, width, height);
        let mut single = shot.clone();
        single.impostor_distance = 0.0;
        let (image, stats) = render(&scene, &meshes, &single, width, height);
        assert_eq!(stats.triangles, 2, "the body was not drawn as a quad");
        assert_eq!(stats.bodies, 1);

        let mut bounds = (u32::MAX, u32::MAX, 0_u32, 0_u32);
        let mut drawn = 0_usize;
        let mut mean = [0.0_f32; 3];
        for y in 0..height {
            for x in 0..width {
                let here = image.pixel(x, y);
                let there = reference.pixel(x, y);
                let differs = here
                    .iter()
                    .zip(there.iter())
                    .any(|(left, right)| left.abs_diff(*right) > 2);
                if differs {
                    drawn += 1;
                    bounds = (
                        bounds.0.min(x),
                        bounds.1.min(y),
                        bounds.2.max(x),
                        bounds.3.max(y),
                    );
                    for (channel, value) in mean.iter_mut().zip(here.iter()) {
                        *channel += f32::from(*value) / 255.0;
                    }
                }
            }
        }
        assert!(drawn > 8, "the billboard covered {drawn} pixels");

        // The billboard is 1.75 m tall with its feet on the ground, so its top and bottom are the
        // projections of those two points, and its width is 0.26 of its height. Predicting them from
        // the camera's own matrix is the check that the quad is a body and not a smear.
        let instance = scene.bodies.first().or_die();
        let foot = project(&shot.camera, width, height, instance.position);
        let head = project(
            &shot.camera,
            width,
            height,
            instance.position + Vec3::Y * 1.75 * instance.scale,
        );
        let left = num::u32_to_f32(bounds.0);
        let top = num::u32_to_f32(bounds.1);
        let right = num::u32_to_f32(bounds.2);
        let bottom = num::u32_to_f32(bounds.3);
        let measured_height = bottom - top + 1.0;
        let measured_width = right - left + 1.0;
        let expected_height = (foot.1 - head.1).abs() + 1.0;
        assert!(
            (measured_height - expected_height).abs() < 2.5,
            "the billboard is {measured_height} px tall, the body projects to {expected_height}"
        );
        assert!(
            (measured_width / measured_height - 0.26).abs() < 0.12,
            "the billboard is {} wide for its height, not a quarter of a body",
            measured_width / measured_height
        );
        assert!(
            (foot.1 - bottom).abs() < 2.5,
            "the billboard's feet are at {bottom}, the body stands at {}",
            foot.1
        );
        assert!(
            (head.1 - top).abs() < 2.5,
            "the billboard's head is at {top}, the body reaches {}",
            head.1
        );
        assert!(
            (foot.0 - f32::midpoint(left, right)).abs() < 2.5,
            "the billboard is off the body's axis"
        );

        // The colour is the asset's material times the instance tint, so a billboard beside a mesh
        // of the same body is the same colour: the shade differs, the hue does not.
        let model = assets::model(instance.model).or_die();
        let expected = [
            model.material[0] * instance.tint[0],
            model.material[1] * instance.tint[1],
            model.material[2] * instance.tint[2],
        ];
        // Only the pixels the quad actually covered, so sky inside the bounding box does not dilute
        // the average.
        for channel in &mut mean {
            *channel /= num::usize_to_f32(drawn);
        }
        let peak = mean.iter().copied().fold(0.0_f32, f32::max).max(1.0e-6);
        let wanted_peak = expected.iter().copied().fold(0.0_f32, f32::max).max(1.0e-6);
        for (channel, reference) in mean.iter().zip(expected.iter()) {
            let ratio = channel / peak;
            let wanted = reference / wanted_peak;
            assert!(
                (ratio - wanted).abs() < 0.15,
                "the billboard's hue is {ratio:.2} where the material wants {wanted:.2}"
            );
        }
    }

    #[test]
    fn the_band_draws_a_body_twice_between_its_edges() {
        let (_, scene, meshes, shot) = avenue(8, 6.0, 6.0);
        let trace = |threshold: f32, band: f32| {
            let mut shot = shot.clone();
            shot.impostor_distance = threshold;
            shot.impostor_band = band;
            let (_, stats) = render(&scene, &meshes, &shot, 200, 140);
            (stats.bodies, stats.triangles)
        };
        let (bodies, detailed) = trace(f32::INFINITY, 0.0);
        assert_eq!(bodies, 8);
        let (_, quads) = trace(0.0, 0.0);
        assert_eq!(quads, 2 * bodies, "a billboard is not two triangles");
        assert!(detailed > quads + 1000);

        // The bodies sit at 6.24, 12.1, 18.1, 24.1 ... metres from the eye. A switch at 24 m with no
        // band draws three meshes and five quads.
        let (_, hard) = trace(24.0, 0.0);
        // An eight-metre band puts the 18.1 m body inside it, so exactly one body pays for both
        // representations and the frame grows by exactly one quad.
        let (_, banded) = trace(24.0, 8.0);
        assert_eq!(
            banded,
            hard + 2,
            "one body in the band should add one quad: {banded} against {hard}"
        );
        // A band that reaches the nearer bodies adds a quad for each: at a 40 m switch with a 12 m
        // band, the bodies at 30.1 and 36.0 m are inside it, and the ones past 40 m stay quads.
        let (_, far_hard) = trace(40.0, 0.0);
        let (_, far_banded) = trace(40.0, 12.0);
        assert_eq!(far_banded, far_hard + 4);
        // A band wider than the switch fades in every body the switch keeps: three meshes and eight
        // quads, so the frame is the hard-switch frame plus a quad for each of the three.
        let (bodies, degenerate) = trace(24.0, 400.0);
        assert_eq!(bodies, 8, "a wide band lost a body");
        assert_eq!(degenerate, hard + 6);
    }

    /// Pixels where two frames differ by more than a rounding step.
    fn mask_between(left: &Image, right: &Image) -> Vec<bool> {
        let mut mask = Vec::with_capacity((left.width * left.height) as usize);
        for y in 0..left.height {
            for x in 0..left.width {
                mask.push(
                    left.pixel(x, y)
                        .iter()
                        .zip(right.pixel(x, y).iter())
                        .any(|(a, b)| a.abs_diff(*b) > 2),
                );
            }
        }
        mask
    }

    #[test]
    fn the_band_is_a_ramp_and_not_a_step() {
        // One body, a camera that walks away from it, and the two representations rendered on their
        // own for comparison. What a coverage fade promises is structural, and that is what is
        // checked: the band touches only the pixels its own body covers, every pixel on the quad's
        // footprint is one representation or the other and never a mixture, and the share that has
        // switched to the quad rises with distance.
        let (_, scene, meshes, base) = avenue(1, 20.0, 1.0);
        let render_from = |distance: f32, threshold: f32, band: f32| {
            let mut shot = base.clone();
            // The body is twenty metres down -Z from the avenue's eye; move the eye to the distance
            // under test.
            shot.camera.eye = base.camera.eye + Vec3::new(0.0, 0.0, distance - 20.0);
            shot.impostor_distance = threshold;
            shot.impostor_band = band;
            let (image, stats) = render(&scene, &meshes, &shot, 160, 160);
            (image, stats)
        };
        let (nothing, _) = {
            let mut shot = base.clone();
            shot.max_bodies = 0;
            render(&scene, &meshes, &shot, 160, 160)
        };
        let same = |left: &Image, right: &Image, x: u32, y: u32| {
            left.pixel(x, y)
                .iter()
                .zip(right.pixel(x, y).iter())
                .all(|(a, b)| a.abs_diff(*b) <= 2)
        };

        let mut previous_fraction = 0.0_f32;
        for distance in [16.05_f32, 18.0, 20.0, 22.0, 23.9] {
            let (mesh_only, mesh_stats) = render_from(distance, f32::INFINITY, 0.0);
            let (billboard_only, quad_stats) = render_from(distance, 0.0, 0.0);
            let (faded, stats) = render_from(distance, 24.0, 8.0);
            assert_eq!(stats.bodies, 1, "the body vanished at {distance} m");
            // A fading body pays for both representations.
            assert_eq!(stats.triangles, mesh_stats.triangles + 2);
            assert!(quad_stats.triangles < mesh_stats.triangles);

            let quad_pixels = mask_between(&billboard_only, &nothing);
            let body_pixels = mask_between(&mesh_only, &nothing);
            let mut switched = 0;
            let mut blended = 0;
            let mut holes = 0;
            for y in 0..faded.height {
                for x in 0..faded.width {
                    let index = (y as usize) * (faded.width as usize) + (x as usize);
                    let on_quad = quad_pixels.get(index).copied().unwrap_or(false);
                    let on_body = body_pixels.get(index).copied().unwrap_or(false);
                    // Nothing outside the body's own pixels may change.
                    if !on_quad && !on_body {
                        assert!(
                            same(&faded, &mesh_only, x, y),
                            "the fade changed a pixel at {x},{y} that no body covers"
                        );
                    }
                    if !on_quad {
                        continue;
                    }
                    if same(&faded, &billboard_only, x, y) {
                        switched += 1;
                    } else if same(&faded, &mesh_only, x, y) {
                        // Still the mesh: the quad has not claimed this pixel yet.
                    } else if same(&faded, &nothing, x, y) {
                        // A hole: the mesh gave this pixel up before the quad took it. That is what a
                        // coverage dissolve does at its edges, and it is why the shipped views leave
                        // the band at zero.
                        holes += 1;
                    } else {
                        blended += 1;
                    }
                }
            }
            assert_eq!(
                blended, 0,
                "{blended} pixels at {distance} m are neither representation, so something blended"
            );
            // At the inner edge the body is still its mesh; at the outer edge it is its quad. In
            // between, the two representations are close enough in colour that "which of the two is
            // this pixel" stops being decidable, so the ramp is only counted past 20 m, where they
            // separate.
            let coverage = ((distance - 16.0) / 8.0).clamp(0.0, 1.0);
            let footprint = quad_pixels.iter().filter(|seen| **seen).count().max(1);
            // The body is the mixture, and only a mixture: half way through the band about half the
            // pixels have changed hands. A hard switch would read as none or all, which is what this
            // rejects.
            if (0.4..0.6).contains(&coverage) {
                assert!(
                    switched >= footprint / 4 && switched <= footprint * 3 / 4,
                    "at {distance} m {switched} of {footprint} pixels had switched at coverage \
                     {coverage}, which is a switch and not a fade"
                );
            }
            if (0.7..0.8).contains(&coverage) {
                assert!(
                    switched >= footprint / 2,
                    "at {distance} m only {switched} of {footprint} pixels had switched at \
                     coverage {coverage}"
                );
            }
            if coverage > 0.95 {
                assert!(
                    switched >= footprint * 19 / 20,
                    "at {distance} m only {switched} of {footprint} pixels had switched"
                );
            }
            if distance >= 20.0 {
                // Fractions, not counts: the quad shrinks as the body recedes, so the number of
                // pixels it covers falls even as its share of them rises.
                let fraction = num::usize_to_f32(switched) / num::usize_to_f32(footprint);
                assert!(
                    fraction >= previous_fraction - 0.01,
                    "the ramp went backwards at {distance} m: {fraction:.3} after {previous_fraction:.3}"
                );
                previous_fraction = fraction;
            }
            // A dissolve opens holes at its own edges; it must not open a third of the body.
            assert!(
                holes <= footprint / 3,
                "{holes} of {footprint} pixels at {distance} m are holes"
            );
            // A stippled frame is still deterministic.
            let (again, _) = render_from(distance, 24.0, 8.0);
            assert_eq!(faded.fingerprint(), again.fingerprint());
        }
    }

    #[test]
    fn the_shipped_views_draw_bodies_at_full_detail() {
        let (city, _scene, bodies) = sample();
        let shipped = shots(&city, &bodies);
        assert_eq!(shipped.len(), 5, "a view went missing");
        for shot in &shipped {
            assert!(
                shot.impostor_distance.is_infinite(),
                "{} draws impostors by default, which would change the PNG it writes",
                shot.name
            );
            assert_eq!(
                shot.impostor_band.to_bits(),
                0.0_f32.to_bits(),
                "{} fades by default",
                shot.name
            );
            let budgeted = crate::session::Budget::default().apply(shot);
            assert_eq!(budgeted.impostor_distance.to_bits(), 24.0_f32.to_bits());
            assert_eq!(budgeted.impostor_band.to_bits(), 8.0_f32.to_bits());
            // The budget is five drawing knobs: framing, lighting and the note travel with the view,
            // because a flight out of the street fog must not take the fog with it.
            assert_eq!(
                budgeted.camera.eye.to_array().map(f32::to_bits),
                shot.camera.eye.to_array().map(f32::to_bits)
            );
            assert_eq!(budgeted.camera.fov_y.to_bits(), shot.camera.fov_y.to_bits());
            assert_eq!(budgeted.name, shot.name);
            assert_eq!(budgeted.note, shot.note);
        }
    }

    #[test]
    fn the_street_view_sees_bodies_rather_than_only_buildings() {
        let city = CityLayout::generate(SimSeed::new(7), 900.0);
        let centre = city
            .districts
            .iter()
            .find(|district| district.class == crate::city::SettlementClass::Large)
            .map_or(Vec3::ZERO, |district| district.centre);
        let bodies: Vec<crate::scene::Body> = (0..40_u64)
            .map(|index| crate::scene::Body {
                archetype: bevy_flock::domain::temperament::Archetype::Urchin,
                position: centre + Vec3::new(num::u64_to_f32(index % 8) * 2.0 - 8.0, 0.0, -6.0),
                yaw: 0.0,
                appearance_seed: index,
            })
            .collect();
        let scene = Scene::build(&city, &bodies);
        let meshes = Meshes::load().or_die();
        let shot = shots(&city, &bodies)
            .into_iter()
            .find(|shot| shot.name == "street")
            .or_die();
        let (image, stats) = render(&scene, &meshes, &shot, 320, 200);
        assert!(stats.bodies > 0, "the street view drew no people at all");
        assert!(stats.coverage > 0.5, "the street view is mostly sky");
        // A frame with people in it has more distinct colours than a frame of walls: the check is
        // variety, not a particular hue, because the tint depends on the archetype's palette.
        let mut seen = std::collections::BTreeSet::new();
        for y in 0..image.height {
            for x in 0..image.width {
                let pixel = image.pixel(x, y);
                seen.insert([pixel[0] / 8, pixel[1] / 8, pixel[2] / 8]);
            }
        }
        assert!(
            seen.len() > 40,
            "the street view has only {} colours",
            seen.len()
        );
    }

    #[test]
    fn a_skyline_render_finishes_and_fills_the_frame() {
        let (city, scene, bodies) = sample();
        let meshes = Meshes::load().or_die();
        let shot = shots(&city, &bodies)
            .into_iter()
            .find(|shot| shot.name == "skyline")
            .or_die();
        let (_image, stats) = render(&scene, &meshes, &shot, 320, 200);
        assert!(
            stats.blocks > 100,
            "the skyline drew {} ruins",
            stats.blocks
        );
        assert!(stats.coverage > 0.3, "the skyline is mostly sky");
    }
}
