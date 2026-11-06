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
            ground_reach: 120.0,
            ground_tile: 12.0,
        },
        Shot {
            name: "skyline",
            note: "the whole verge from 620 m: six settlements and the ruins between them",
            camera: camera(
                centre + Vec3::new(-330.0, 208.0, 452.0),
                centre + Vec3::new(-70.0, 6.0, -170.0),
                0.72,
            ),
            atmosphere: skyline,
            body_margin: 0.0,
            draw_distance: 1400.0,
            max_bodies: 900,
            block_distance: 2100.0,
            ground_reach: 2200.0,
            ground_tile: 110.0,
        },
    ]
}

/// A place to stand in the street: 34 m south of the settlement centre, nudged along the street
/// until nothing is standing in the camera's lap.
fn street_vantage(centre: Vec3, bodies: &[Body]) -> Vec3 {
    let base = centre + Vec3::new(0.0, 1.68, 34.0);
    let mut best = base;
    let mut best_gap = f32::NEG_INFINITY;
    for step in -6..=6 {
        let candidate = base + Vec3::new(step as f32 * 2.5, 0.0, 0.0);
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
    for (instance, _) in nearby.into_iter().take(shot.max_bodies) {
        if let Some(mesh) = meshes.get(instance.model) {
            stats.triangles += push_instance(renderer, image, mesh, instance);
            stats.bodies += 1;
        }
    }

    stats
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
    let aspect = width as f32 / height.max(1) as f32;
    let renderer = Renderer::new(&shot.camera, aspect, shot.atmosphere);
    let mut image = Image::new(width, height);
    // Where the camera's eye level cuts the frame, in NDC then in rows: the sky gradient reaches
    // the fog colour exactly there, so the ground plane's far edge has nothing to end against.
    let forward = (shot.camera.target - shot.camera.eye).normalize_or_zero();
    let pitch = forward.y.clamp(-1.0, 1.0).asin();
    let horizon_ndc = -pitch.tan() / (shot.camera.fov_y * 0.5).tan().max(1.0e-4);
    let horizon_row = ((1.0 - (horizon_ndc * 0.5 + 0.5))
        * f32::from(u16::try_from(height).unwrap_or(1)))
    .clamp(0.0, f32::from(u16::try_from(height).unwrap_or(1)))
    .round() as u32;
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
    let tiles = ((reach * 2.0) / tile).ceil() as i32 + 2;
    let half = tiles / 2;
    for row in -half..half {
        for column in -half..half {
            let x = anchor.x + column as f32 * tile;
            let z = anchor.z + row as f32 * tile;
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
        renderer.triangle(
            image,
            &Face {
                vertices: [a, b, c],
                albedo,
                style: Style::Lit,
            },
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
                position: centre + Vec3::new((index % 6) as f32 * 3.0 - 9.0, 0.0, -8.0),
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
                position: centre + Vec3::new((index % 8) as f32 * 2.0 - 8.0, 0.0, -6.0),
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
