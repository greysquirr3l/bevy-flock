//! What the run proved, as a document.
//!
//! Three numbers matter in this crate and all three are here: the digest (did the run repeat), the
//! tier histogram (did the budget spread the thinking), and the wall time (what the frame cost).
//! Everything else is context for reading those three, which is why the report is written by the
//! run rather than typed into a document afterwards — a performance claim in a README rots, a
//! `run.json` from the binary that made it does not.

use crate::behaviour::{self, ArchetypeBehaviour};
use crate::json;
use bevy_flock::adapters::primary::observability::Diagnostics;

/// One finished run.
#[derive(Debug, Clone)]
pub struct RunReport {
    /// The seed the city and the crowd came from.
    pub seed: u64,
    /// Half-width of the city, metres.
    pub extent: f32,
    /// NPCs alive at the end.
    pub population: u32,
    /// Simulation ticks run before the report.
    pub ticks: u64,
    /// Ticks spent waiting for the spawn queue to drain.
    pub spawn_ticks: u64,
    /// Wall time for the measured ticks, milliseconds.
    pub wall_ms: f64,
    /// Milliseconds per tick over the measured window.
    pub ms_per_tick: f64,
    /// The library's state digest (coarse by design: population, tiers, mood, tick).
    pub digest: u64,
    /// A fingerprint of where the crowd is, which the library's digest cannot give.
    pub crowd: u64,
    /// Cognition tiers, reflex first.
    pub tiers: [u32; 4],
    /// Cities: districts, blocks, shelters, pylons, stalls.
    pub counts: [usize; 5],
    /// Model slug, byte length, fingerprint, per shipped asset.
    pub assets: Vec<(String, u64, u64)>,
    /// What each archetype's personality asked of the steering, and what the crowd did with it.
    ///
    /// Empty for a run that produced no bodies; the report then says nothing rather than guessing.
    pub behaviour: Vec<ArchetypeBehaviour>,
}

impl RunReport {
    /// Read the parts the library owns out of a finished app.
    #[must_use]
    pub fn from_diagnostics(diagnostics: &Diagnostics) -> Self {
        Self {
            seed: 0,
            extent: 0.0,
            population: diagnostics.pulse.population,
            ticks: diagnostics.frames,
            spawn_ticks: 0,
            wall_ms: 0.0,
            ms_per_tick: 0.0,
            digest: diagnostics.digest(),
            crowd: 0,
            tiers: diagnostics.pulse.tiers,
            counts: [0; 5],
            assets: Vec::new(),
            behaviour: Vec::new(),
        }
    }

    /// The tier shares as a percentage string, reflex first.
    #[must_use]
    pub fn tier_shares(&self) -> [f32; 4] {
        let total: u32 = self.tiers.iter().sum();
        if total == 0 {
            return [0.0; 4];
        }
        let mut shares = [0.0_f32; 4];
        for (index, count) in self.tiers.iter().enumerate() {
            if let Some(slot) = shares.get_mut(index) {
                *slot = *count as f32 / total as f32;
            }
        }
        shares
    }

    /// A few lines a person can read without opening the JSON.
    #[must_use]
    pub fn headline(&self) -> String {
        let shares = self.tier_shares();
        format!(
            "crowd   {} NPCs · {} ticks · {:.2} ms/tick\n\
             tiers   {:.0}% reflex / {:.0}% habit / {:.0}% social / {:.0}% deliberate\n\
             digest  0x{:016x} (coarse) · crowd 0x{:016x}",
            self.population,
            self.ticks,
            self.ms_per_tick,
            shares[0] * 100.0,
            shares[1] * 100.0,
            shares[2] * 100.0,
            shares[3] * 100.0,
            self.digest,
            self.crowd
        )
    }

    /// Render the report as JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let tiers: Vec<String> = ["reflex", "habit", "social", "deliberate"]
            .iter()
            .zip(self.tiers)
            .map(|(name, count)| {
                json::inline(&[("tier", json::string(name)), ("count", count.to_string())])
            })
            .collect();
        let assets: Vec<String> = self
            .assets
            .iter()
            .map(|(slug, bytes, hash)| {
                json::inline(&[
                    ("slug", json::string(slug)),
                    ("bytes", bytes.to_string()),
                    ("fingerprint", format!("\"0x{hash:016x}\"")),
                ])
            })
            .collect();
        json::object(
            &[
                ("format", json::string("bevy-flock-demo-run/1")),
                ("seed", self.seed.to_string()),
                ("extent", json::number(self.extent, 2)),
                ("population", self.population.to_string()),
                ("ticks", self.ticks.to_string()),
                ("spawn_ticks", self.spawn_ticks.to_string()),
                ("wall_ms", json::number(self.wall_ms as f32, 3)),
                ("ms_per_tick", json::number(self.ms_per_tick as f32, 4)),
                ("digest", format!("\"0x{:016x}\"", self.digest)),
                ("crowd_fingerprint", format!("\"0x{:016x}\"", self.crowd)),
                ("tiers", json::array_block(&tiers, 2)),
                (
                    "city",
                    json::inline(&[
                        ("districts", self.counts[0].to_string()),
                        ("blocks", self.counts[1].to_string()),
                        ("shelters", self.counts[2].to_string()),
                        ("pylons", self.counts[3].to_string()),
                        ("stalls", self.counts[4].to_string()),
                    ]),
                ),
                ("assets", json::array_block(&assets, 2)),
                ("behaviour", behaviour::to_json_block(&self.behaviour)),
            ],
            0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RunReport {
        RunReport {
            seed: 3,
            extent: 900.0,
            population: 1_000,
            ticks: 600,
            spawn_ticks: 4,
            wall_ms: 210.5,
            ms_per_tick: 0.35,
            digest: 0xdead_beef,
            crowd: 0xfeed_face,
            tiers: [700, 200, 80, 20],
            counts: [6, 120, 10, 10, 40],
            assets: vec![("gatewarden".to_owned(), 1_048_576, 0x1234)],
            behaviour: Vec::new(),
        }
    }

    #[test]
    fn shares_are_of_the_whole_crowd() {
        let shares = sample().tier_shares();
        let total: f32 = shares.iter().sum();
        assert!((total - 1.0).abs() < 1e-5, "shares sum to {total}");
        assert!(shares[0] > shares[3]);
    }

    #[test]
    fn an_empty_crowd_does_not_divide_by_zero() {
        let mut report = sample();
        report.tiers = [0; 4];
        assert_eq!(report.tier_shares(), [0.0; 4]);
        assert!(report.headline().contains("0% reflex"));
    }

    #[test]
    fn the_document_carries_the_digest_and_the_tiers() {
        let text = sample().to_json();
        assert!(
            text.contains("\"digest\": \"0x00000000deadbeef\""),
            "{text}"
        );
        assert!(text.contains("\"tier\": \"deliberate\""), "{text}");
        assert!(
            text.contains("\"fingerprint\": \"0x0000000000001234\""),
            "{text}"
        );
        assert!(text.contains("\"districts\": 6"), "{text}");
    }
}
