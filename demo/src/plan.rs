//! The plan: a top-down drawing of the city and the crowd in it.
//!
//! An SVG rather than a screenshot, because a screenshot needs a renderer and this needs a browser.
//! It is also the artefact that makes the layout arguable: a reader can see that the shelters stand
//! in the ruins, that the pylons sit at gates and junctions, and that the crowd is in the
//! settlements rather than spread over empty ground — which is three of the invariants the tests
//! assert.
//!
//! Everything is drawn in world metres and scaled once, so the drawing cannot disagree with the
//! simulation about where anything is.

use crate::city::CityLayout;
use crate::scene::{Body, Scene};
use bevy_flock::domain::temperament::Archetype;
use bevy_math::Vec3;
use std::fmt::Write as _;

/// Canvas size in pixels (square).
pub const CANVAS: f32 = 1400.0;
const PAD: f32 = 44.0;

/// Colours, named so the legend and the drawing cannot drift apart.
const INK: &str = "#efe7da";
const MUTED: &str = "#9a9082";
const GROUND: &str = "#14120f";
const ROAD: &str = "#262220";
const BLOCK: &str = "#39322a";
const SETTLEMENT: &str = "#2f6f68";
const SHELTER: &str = "#d8c07a";
const PYLON: &str = "#cfd6dd";
const STALL: &str = "#c08a5a";

/// Escape the five characters that would otherwise break the document.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Map a world point to canvas pixels, with `+z` downward as a plan reads.
fn project(point: Vec3, extent: f32) -> (f32, f32) {
    let span = CANVAS - PAD * 2.0;
    let scale = span / (extent * 2.0);
    (
        PAD + (point.x + extent) * scale,
        PAD + (point.z + extent) * scale,
    )
}

/// Pixels per metre at this extent.
fn scale_of(extent: f32) -> f32 {
    (CANVAS - PAD * 2.0) / (extent * 2.0)
}

fn archetype_colour(archetype: Archetype) -> &'static str {
    match archetype {
        Archetype::Guard => "#7fb3ab",
        Archetype::Merchant | Archetype::Noble => "#c9a06a",
        Archetype::Urchin | Archetype::Wanderer => "#9c9488",
    }
}

/// Render the plan.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the plan is drawn in painting order -- roads, ruins, settlements, props, crowd, \
              legend -- and each layer is three to eight lines; the alternative is passing a dozen \
              mutable buffers between helpers to keep a lint quiet"
)]
pub fn render(city: &CityLayout, bodies: &[Body], digest: u64, ticks: u64) -> String {
    let scale = scale_of(city.extent);
    let mut svg = String::with_capacity(64 * 1024);
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" height="{CANVAS}" viewBox="0 0 {CANVAS} {CANVAS}" font-family="ui-monospace, SFMono-Regular, Menlo, monospace">
<rect width="{CANVAS}" height="{CANVAS}" fill="{GROUND}"/>
"#
    );

    // Roads first: everything else stands on them.
    let _ = writeln!(
        svg,
        r#"<g stroke="{ROAD}" stroke-width="{}">"#,
        city.road_width * scale
    );
    let cells = (city.extent * 2.0 / city.block_pitch).round().max(1.0) as i32;
    for index in 0..=cells {
        let metre =
            f32::from(i16::try_from(index).unwrap_or(i16::MAX)) * city.block_pitch - city.extent;
        let (x, _) = project(Vec3::new(metre, 0.0, 0.0), city.extent);
        let (_, z) = project(Vec3::new(0.0, 0.0, metre), city.extent);
        let _ = writeln!(
            svg,
            r#"<line x1="{x:.1}" y1="{PAD}" x2="{x:.1}" y2="{:.1}"/>"#,
            CANVAS - PAD
        );
        let _ = writeln!(
            svg,
            r#"<line x1="{PAD}" y1="{z:.1}" x2="{:.1}" y2="{z:.1}"/>"#,
            CANVAS - PAD
        );
    }
    let _ = writeln!(svg, "</g>");

    // Ruined blocks, brighter where they still stand.
    let _ = writeln!(svg, "<g>");
    for block in &city.blocks {
        let (x, z) = project(block.centre, city.extent);
        let width = block.size.x * scale;
        let depth = block.size.z * scale;
        // Height and survival, not one number: a standing tower and a surviving corner should not
        // be the same grey.
        let presence = (block.size.y / 26.0).clamp(0.0, 1.0);
        let opacity = 0.16 + (1.0 - block.ruin) * 0.32 + presence * 0.28;
        let _ = writeln!(
            svg,
            r##"<rect x="{:.1}" y="{:.1}" width="{width:.1}" height="{depth:.1}" fill="{BLOCK}" fill-opacity="{opacity:.2}" stroke="#00000055" stroke-width="0.6"/>"##,
            x - width * 0.5,
            z - depth * 0.5
        );
    }
    let _ = writeln!(svg, "</g>");

    // Settlements.
    let _ = writeln!(svg, "<g>");
    for district in &city.districts {
        let (x, z) = project(district.centre, city.extent);
        let radius = district.radius * scale;
        let _ = writeln!(
            svg,
            r#"<circle cx="{x:.1}" cy="{z:.1}" r="{radius:.1}" fill="{SETTLEMENT}" fill-opacity="0.13" stroke="{SETTLEMENT}" stroke-opacity="0.55" stroke-width="1.4" stroke-dasharray="7 5"/>"#
        );
        let _ = writeln!(
            svg,
            r#"<text x="{x:.1}" y="{:.1}" fill="{INK}" font-size="15" text-anchor="middle" opacity="0.9">{}</text>"#,
            z - radius - 9.0,
            escape(&district.slug)
        );
        let _ = writeln!(
            svg,
            r#"<text x="{x:.1}" y="{:.1}" fill="{MUTED}" font-size="11.5" text-anchor="middle">{}{}</text>"#,
            z - radius + 5.0,
            district.class.label(),
            if district.guild { ", guild" } else { "" }
        );
    }
    let _ = writeln!(svg, "</g>");

    // Props, drawn in world order so the label pass is stable.
    let _ = writeln!(svg, "<g>");
    for shelter in &city.shelters {
        let (x, z) = project(shelter.position, city.extent);
        let size = 11.0;
        let _ = writeln!(
            svg,
            r#"<rect x="{:.1}" y="{:.1}" width="{size}" height="{size}" fill="{SHELTER}" fill-opacity="0.9" transform="rotate({:.1} {x:.1} {z:.1})"/>"#,
            x - size * 0.5,
            z - size * 0.5,
            shelter.yaw.to_degrees()
        );
    }
    for stall in &city.stalls {
        let (x, z) = project(stall.position, city.extent);
        let _ = writeln!(
            svg,
            r#"<rect x="{:.1}" y="{:.1}" width="5.5" height="5.5" fill="{STALL}" fill-opacity="0.85"/>"#,
            x - 2.75,
            z - 2.75
        );
    }
    for pylon in &city.pylons {
        let (x, z) = project(pylon.position, city.extent);
        let _ = writeln!(
            svg,
            r#"<polygon points="{x:.1},{:.1} {:.1},{:.1} {:.1},{:.1}" fill="{PYLON}" fill-opacity="0.95"/>"#,
            z - 7.0,
            x - 5.0,
            z + 4.0,
            x + 5.0,
            z + 4.0
        );
    }
    let _ = writeln!(svg, "</g>");

    // The crowd. Drawn last so it reads on top of the city it stands in.
    let _ = writeln!(svg, "<g>");
    for body in bodies {
        let (x, z) = project(body.position, city.extent);
        let _ = writeln!(
            svg,
            r#"<circle cx="{x:.1}" cy="{z:.1}" r="1.7" fill="{}" fill-opacity="0.92"/>"#,
            archetype_colour(body.archetype)
        );
    }
    let _ = writeln!(svg, "</g>");

    legend_and_footer(&mut svg, city, bodies, digest, ticks);
    let _ = writeln!(svg, "</svg>");
    svg
}

fn legend_and_footer(
    svg: &mut String,
    city: &CityLayout,
    bodies: &[Body],
    digest: u64,
    ticks: u64,
) {
    let x = PAD;
    let mut y = CANVAS - PAD - 74.0;
    let _ = writeln!(svg, r#"<g font-size="12.5" fill="{INK}">"#);
    let _ = writeln!(
        svg,
        r#"<text x="{x}" y="{y}" font-size="19">Ashfall Verge</text>"#
    );
    y += 20.0;
    let _ = writeln!(
        svg,
        r#"<text x="{x}" y="{y}" fill="{MUTED}" font-size="12">seed {} · {ticks} ticks · digest 0x{digest:016x}</text>"#,
        city.seed.get()
    );
    y += 22.0;

    let entries: [(&str, &str, &str); 5] = [
        (SHELTER, "square", "shelter"),
        (PYLON, "triangle", "wayfinding"),
        (STALL, "dot", "stall"),
        (SETTLEMENT, "ring", "settlement"),
        (BLOCK, "block", "ruin"),
    ];
    let mut line_x = x;
    for (colour, shape, label) in entries {
        let mark = match shape {
            "square" => format!(
                r#"<rect x="{line_x}" y="{:.1}" width="9" height="9" fill="{colour}"/>"#,
                y - 9.0
            ),
            "triangle" => format!(
                r#"<polygon points="{line_x},{:.1} {:.1},{:.1} {:.1},{:.1}" fill="{colour}"/>"#,
                y - 10.0,
                line_x - 5.0,
                y - 1.0,
                line_x + 5.0,
                y - 1.0
            ),
            "dot" => format!(
                r#"<circle cx="{:.1}" cy="{:.1}" r="4.5" fill="{colour}"/>"#,
                line_x + 4.5,
                y - 4.5
            ),
            "ring" => format!(
                r#"<circle cx="{:.1}" cy="{:.1}" r="6" fill="none" stroke="{colour}" stroke-dasharray="3 2"/>"#,
                line_x + 5.0,
                y - 4.0
            ),
            _ => format!(
                r#"<rect x="{line_x}" y="{:.1}" width="11" height="8" fill="{BLOCK}" fill-opacity="0.8"/>"#,
                y - 8.0
            ),
        };
        let _ = writeln!(svg, "{mark}");
        let _ = writeln!(
            svg,
            r#"<text x="{:.1}" y="{y}" fill="{MUTED}">{label}</text>"#,
            line_x + 17.0
        );
        line_x += 17.0 + 8.0 * label.len() as f32 + 26.0;
    }
    y += 20.0;
    let guards = bodies
        .iter()
        .filter(|body| body.archetype == Archetype::Guard)
        .count();
    let aerial = bodies
        .iter()
        .filter(|body| matches!(body.archetype, Archetype::Merchant | Archetype::Noble))
        .count();
    let scavengers = bodies.len() - guards - aerial;
    let _ = writeln!(
        svg,
        r#"<text x="{x}" y="{y}" fill="{MUTED}" font-size="12">{} drawn · {guards} watch · {aerial} aerial · {scavengers} scavenging · {} shelters · {} pylons</text>"#,
        bodies.len(),
        city.shelters.len(),
        city.pylons.len()
    );
    let _ = writeln!(svg, "</g>");
}

/// A short textual summary of the layout, for a terminal or a log line.
#[must_use]
pub fn summary(city: &CityLayout) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "city    {} districts, {} blocks, {} shelters, {} pylons, {} stalls over {:.0} m",
        city.districts.len(),
        city.blocks.len(),
        city.shelters.len(),
        city.pylons.len(),
        city.stalls.len(),
        city.extent * 2.0
    );
    for district in &city.districts {
        let _ = writeln!(
            out,
            "  {:<14} {:<7} r={:<5.0} {:.0}% of the crowd{}",
            district.slug,
            district.class.label(),
            district.radius,
            district.population_share * 100.0,
            if district.guild { "  (guild)" } else { "" }
        );
    }
    out
}

/// Whether a scene would draw anything, used by the smoke test.
#[must_use]
pub fn scene_has_content(scene: &Scene) -> bool {
    !scene.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_flock::domain::ids::SimSeed;
    use bevy_flock::testkit::OrDie;

    fn city() -> CityLayout {
        CityLayout::generate(SimSeed::new(5), 800.0)
    }

    #[test]
    fn the_document_is_well_formed() {
        let layout = city();
        let svg = render(&layout, &[], 0, 10);
        assert!(svg.starts_with("<svg xmlns"));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<svg").count(), 1);
        // Every opening group is closed.
        assert_eq!(svg.matches("<g").count(), svg.matches("</g>").count());
    }

    #[test]
    fn settlement_names_are_escaped() {
        let mut layout = city();
        if let Some(district) = layout.districts.first_mut() {
            district.slug = "gate<&>row".to_owned();
        }
        let svg = render(&layout, &[], 0, 0);
        assert!(
            svg.contains("gate&lt;&amp;&gt;row"),
            "unescaped name in the plan"
        );
    }

    #[test]
    fn a_body_lands_where_the_city_says() {
        let layout = city();
        let district = layout.districts.first().or_die();
        let body = Body {
            archetype: Archetype::Guard,
            position: district.centre,
            yaw: 0.0,
            appearance_seed: 9,
        };
        let svg = render(&layout, &[body], 0, 0);
        let (x, z) = project(district.centre, layout.extent);
        assert!(
            svg.contains(&format!(r#"<circle cx="{x:.1}" cy="{z:.1}" r="1.7""#)),
            "the body is not drawn at its own centre"
        );
    }

    #[test]
    fn the_plan_states_the_seed_and_the_digest() {
        let layout = city();
        let svg = render(&layout, &[], 0xabcd, 42);
        assert!(svg.contains("digest 0x000000000000abcd"));
        assert!(svg.contains("42 ticks"));
        assert!(svg.contains(&format!("seed {}", layout.seed.get())));
    }

    #[test]
    fn class_counts_are_reported() {
        let layout = city();
        let text = summary(&layout);
        assert!(text.contains("gate-row"));
        assert!(text.contains("large"));
        assert!(text.contains("(guild)"));
        let total: f32 = layout
            .districts
            .iter()
            .map(|district| district.population_share)
            .sum();
        assert!((total - 1.0).abs() < 1e-4, "shares sum to {total}");
    }
}
