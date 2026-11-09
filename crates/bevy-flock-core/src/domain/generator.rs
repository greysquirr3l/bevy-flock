//! Procedural NPC generation: names, looks and backgrounds from a seed.
//!
//! Generation is a pure function of `(seed, index, &ArchetypeTable)`. It reads no
//! clock, touches no file, allocates nothing but the name strings, and never asks
//! the platform for a random number: the same triple produces the same blueprint
//! on every machine and in every process, and the order the game generates them
//! in does not matter.
//!
//! That property is what lets a server and a replay agree about who lives in a
//! city nobody has visited yet. Ten thousand inhabitants of an ambient district
//! are not simulated until someone walks in, but they already exist, and they
//! exist identically for everyone holding the seed.
//!
//! Appearance is a seed plus a tag list, never an asset path: this crate does not
//! know what a mesh is, and a game that puts a mesh in a data file has made its
//! content pipeline the library's problem.

use crate::domain::ids::SimSeed;
use crate::domain::num;
use crate::domain::rng::RngStream;
use crate::domain::temperament::{Archetype, Temperament, Weight};
use smallvec::SmallVec;

/// The most tags one blueprint carries.
pub const MAX_TAGS: usize = 4;

/// The syllable pools a name is drawn from.
///
/// A style is a whole naming culture, not a list of names: onsets, vowels and
/// codas combine into far more given names than any curated list could hold, and
/// the surname list supplies the family or trade name on top.
///
/// ```
/// use bevy_flock_core::domain::generator::NameStyle;
/// use bevy_flock_core::domain::ids::SimSeed;
/// use bevy_flock_core::domain::rng::RngStream;
///
/// let mut rng = RngStream::new(SimSeed::new(7), 1);
/// let name = NameStyle::Highland.compose(&mut rng, None);
/// assert!(!name.given.is_empty());
/// assert!(!name.family.is_empty());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NameStyle {
    /// Hard consonants, clipped endings.
    Nordic,
    /// Open vowels, latinate endings.
    Latin,
    /// A trading city: short, mixed, slightly foreign.
    TradeCity,
    /// The highlands: broad vowels, gutturals, surnames taken from places.
    Highland,
}

impl NameStyle {
    /// Every style, in a fixed order.
    pub const ALL: [Self; 4] = [Self::Nordic, Self::Latin, Self::TradeCity, Self::Highland];

    /// The style's fixed pool of onsets.
    #[must_use]
    pub const fn onsets(self) -> &'static [&'static str] {
        match self {
            Self::Nordic => &[
                "B", "Br", "D", "E", "F", "G", "H", "Hj", "K", "Kn", "L", "M", "N", "R", "S", "Sk",
                "St", "T", "Th", "V",
            ],
            Self::Latin => &[
                "A", "C", "Cl", "D", "F", "Fl", "G", "L", "M", "N", "P", "Qu", "R", "S", "T", "V",
                "Val", "Viv",
            ],
            Self::TradeCity => &[
                "A", "B", "D", "F", "H", "J", "K", "M", "N", "P", "R", "S", "T", "Y", "Z", "Sh",
                "Mar", "Sar",
            ],
            Self::Highland => &[
                "B", "Br", "C", "Cr", "D", "Dr", "F", "G", "Gr", "H", "L", "Ll", "M", "N", "R",
                "S", "T", "Th",
            ],
        }
    }

    /// The style's fixed pool of vowels.
    #[must_use]
    pub const fn vowels(self) -> &'static [&'static str] {
        match self {
            Self::Nordic => &["a", "e", "i", "o", "u", "au", "ei", "y"],
            Self::Latin => &["a", "e", "i", "o", "u", "ae", "ia", "io"],
            Self::TradeCity => &["a", "e", "i", "o", "u", "ai", "ea"],
            Self::Highland => &[
                "a", "e", "i", "o", "u", "ai", "ei", "ia", "io", "oa", "ua", "ea",
            ],
        }
    }

    /// The style's fixed pool of codas.
    #[must_use]
    pub const fn codas(self) -> &'static [&'static str] {
        match self {
            Self::Nordic => &["d", "f", "g", "k", "l", "m", "n", "r", "s", "st", "th", "v"],
            Self::Latin => &["", "n", "s", "us", "a", "ia", "ius", "or"],
            Self::TradeCity => &["", "n", "r", "s", "l", "m", "th", "ck"],
            Self::Highland => &[
                "", "ch", "dh", "g", "gh", "l", "ll", "m", "n", "nn", "r", "rd", "th", "s",
            ],
        }
    }

    /// Surnames: families, trades and places.
    #[must_use]
    pub const fn surnames(self) -> &'static [&'static str] {
        match self {
            Self::Nordic => &[
                "Ahlgren",
                "Berg",
                "Dahl",
                "Eklund",
                "Fjord",
                "Halvard",
                "Holm",
                "Kettil",
                "Lindqvist",
                "Nordberg",
                "Rask",
                "Sköld",
                "Strand",
                "Sund",
                "Thorvald",
                "Vikander",
            ],
            Self::Latin => &[
                "Aurelius",
                "Cassia",
                "Corvinus",
                "Fabia",
                "Gratian",
                "Livianus",
                "Marcellus",
                "Nepos",
                "Octavia",
                "Priscus",
                "Sabina",
                "Severus",
                "Tullius",
                "Valerius",
            ],
            Self::TradeCity => &[
                "Abrams", "Bexley", "Callow", "Dray", "Fenwick", "Gale", "Harrow", "Jessup",
                "Larkin", "Mercer", "Nott", "Pike", "Quill", "Rook", "Sable", "Thorne", "Vance",
                "Wick",
            ],
            Self::Highland => &[
                "Balmorran",
                "Braemar",
                "Cauldwell",
                "Craigmore",
                "Dellraven",
                "Dunmore",
                "Elgin",
                "Farrow",
                "Fernbrake",
                "Garroway",
                "Glenmorgan",
                "Glenrigh",
                "Hartstone",
                "Heatherby",
                "Highfern",
                "Inverloch",
                "Isleworth",
                "Jedburgh",
                "Kilbride",
                "Kilmartin",
                "Kirkmara",
                "Linnheath",
                "Lochran",
                "Moredun",
                "Muirfield",
                "Netherfield",
                "Norvale",
                "Ormsby",
                "Pennan",
                "Quennell",
                "Raeburn",
                "Rosslyn",
                "Stonehaven",
                "Strathmore",
                "Struan",
                "Tarbert",
                "Thistlewood",
                "Torquil",
                "Uist",
                "Westerloch",
                "Yarrowdale",
            ],
        }
    }

    /// Epithets used to break a name collision.
    #[must_use]
    pub const fn epithets(self) -> &'static [&'static str] {
        match self {
            Self::Nordic => &[
                "the Elder",
                "the Younger",
                "of the Fjord",
                "of the Vale",
                "the Quiet",
            ],
            Self::Latin => &[
                "Major",
                "Minor",
                "of the Hill",
                "of the Forum",
                "the Younger",
            ],
            Self::TradeCity => &[
                "the Elder",
                "the Younger",
                "of the Gate",
                "of the Quay",
                "the Quick",
            ],
            Self::Highland => &[
                "of the Brae",
                "of the Glen",
                "of the Loch",
                "the Steadfast",
                "the Wanderer",
            ],
        }
    }

    /// A stable name for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nordic => "nordic",
            Self::Latin => "latin",
            Self::TradeCity => "trade-city",
            Self::Highland => "highland",
        }
    }

    /// Draw a given name from the style's pools.
    fn given_name(self, rng: &mut RngStream) -> String {
        let onsets = self.onsets();
        let vowels = self.vowels();
        let codas = self.codas();
        let mut name = String::with_capacity(14);
        // Two syllables: consonant, vowel, coda, consonant, vowel, coda. The pools are
        // non-empty constants, so the fallback arm is unreachable in practice; it still
        // returns without indexing so the crate can keep `indexing_slicing` denied.
        for syllable in 0..2 {
            if let Some(onset) =
                onsets.get(rng.next_bounded(num::usize_to_u32(onsets.len())) as usize)
            {
                // The first onset opens the name and keeps the capital the pool gave it.
                // Every later onset sits mid-word, where a capital reads as a typo — "SkaNaum"
                // rather than "Skanaur" — so it is lowered.
                if syllable == 0 {
                    name.push_str(onset);
                } else {
                    name.extend(onset.chars().map(|c| c.to_ascii_lowercase()));
                }
            }
            if let Some(vowel) =
                vowels.get(rng.next_bounded(num::usize_to_u32(vowels.len())) as usize)
            {
                name.push_str(vowel);
            }
            // A final syllable always closes; the first one closes half the time, which is
            // what stops every name having the same rhythm.
            if (syllable == 1 || rng.next_bool(0.5))
                && let Some(coda) =
                    codas.get(rng.next_bounded(num::usize_to_u32(codas.len())) as usize)
            {
                name.push_str(coda);
            }
        }
        name
    }

    /// Compose a full name, with an epithet when one is supplied.
    #[must_use]
    pub fn compose(self, rng: &mut RngStream, epithet: Option<&str>) -> Name {
        let given = self.given_name(rng);
        let surnames = self.surnames();
        let family = surnames
            .get(rng.next_bounded(num::usize_to_u32(surnames.len())) as usize)
            .copied()
            .unwrap_or("Nameless")
            .to_owned();
        Name {
            given,
            family,
            epithet: epithet.map(str::to_owned),
        }
    }
}

/// The style written as its name, so a log line reads as prose rather than as a variant.
///
/// ```
/// use bevy_flock_core::domain::generator::NameStyle;
///
/// assert_eq!(NameStyle::Highland.to_string(), "highland");
/// assert_eq!(NameStyle::TradeCity.to_string(), "trade-city");
/// ```
impl core::fmt::Display for NameStyle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A name, split so a game can sort, filter and address people by part.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Name {
    /// The given name.
    pub given: String,
    /// The family, trade or place name.
    pub family: String,
    /// An epithet, present only when a collision had to be broken.
    pub epithet: Option<String>,
}

impl Name {
    /// The name as a game would print it.
    #[must_use]
    pub fn full(&self) -> String {
        self.epithet.as_ref().map_or_else(
            || format!("{} {}", self.given, self.family),
            |epithet| format!("{} {} {}", self.given, self.family, epithet),
        )
    }

    /// The bare given-and-family pair, ignoring any epithet.
    ///
    /// Collision measurement uses this: two names that differ only by epithet are
    /// the same person to a reader who is not paying attention, and the suffix
    /// rule exists precisely because that happens.
    #[must_use]
    pub const fn bare(&self) -> (&str, &str) {
        (self.given.as_str(), self.family.as_str())
    }
}

impl core::fmt::Display for Name {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.full())
    }
}

/// A content tag: a loadout hint, a dialogue category, a faction flavour.
///
/// Deliberately an id and not a string. A game maps ids to its own content; the
/// crate only ever compares them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TagId(u16);

impl TagId {
    /// A tag identifier.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// The raw identifier.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// One weighted archetype, with everything the generator needs to dress it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArchetypeEntry {
    /// Which archetype this row generates.
    pub archetype: Archetype,
    /// Relative frequency. Weights need not sum to anything in particular.
    pub weight: u32,
    /// Tags every member of this archetype carries.
    pub tags: SmallVec<[TagId; MAX_TAGS]>,
    /// A short label a game can show: "gate watch", "salt merchant".
    pub label: &'static str,
}

impl ArchetypeEntry {
    /// A row with no tags.
    #[must_use]
    pub fn new(archetype: Archetype, weight: u32, label: &'static str) -> Self {
        Self {
            archetype,
            weight,
            tags: SmallVec::new(),
            label,
        }
    }

    /// Add a tag, returning the row for chaining.
    #[must_use]
    pub fn with_tag(mut self, tag: TagId) -> Self {
        if self.tags.len() < MAX_TAGS {
            self.tags.push(tag);
        }
        self
    }
}

/// How often each archetype appears, and what it carries.
#[derive(Debug, Clone)]
pub struct ArchetypeTable {
    entries: Vec<ArchetypeEntry>,
    total_weight: u64,
}

impl ArchetypeTable {
    /// A table from weighted rows.
    ///
    /// # Errors
    ///
    /// Returns [`GeneratorError::EmptyTable`] if no rows are given, and
    /// [`GeneratorError::ZeroWeight`] if every row weighs zero, because a table
    /// that cannot produce an archetype would silently generate nothing.
    pub fn new(entries: Vec<ArchetypeEntry>) -> Result<Self, GeneratorError> {
        if entries.is_empty() {
            return Err(GeneratorError::EmptyTable);
        }
        let total_weight: u64 = entries.iter().map(|entry| u64::from(entry.weight)).sum();
        if total_weight == 0 {
            return Err(GeneratorError::ZeroWeight);
        }
        Ok(Self {
            entries,
            total_weight,
        })
    }

    /// The table a fantasy town ships with: mostly working people, a few nobles.
    ///
    /// # Errors
    ///
    /// As [`ArchetypeTable::new`]; this table is a compile-time constant and
    /// cannot fail, so the error can be ignored with `?` or `expect`.
    pub fn standard() -> Result<Self, GeneratorError> {
        Self::new(vec![
            ArchetypeEntry::new(Archetype::Guard, 12, "gate watch").with_tag(TagId::new(1)),
            ArchetypeEntry::new(Archetype::Merchant, 30, "salt merchant").with_tag(TagId::new(2)),
            ArchetypeEntry::new(Archetype::Urchin, 24, "roof runner").with_tag(TagId::new(3)),
            ArchetypeEntry::new(Archetype::Noble, 6, "house heir").with_tag(TagId::new(4)),
            ArchetypeEntry::new(Archetype::Wanderer, 28, "traveller").with_tag(TagId::new(5)),
        ])
    }

    /// The rows.
    #[must_use]
    pub fn entries(&self) -> &[ArchetypeEntry] {
        &self.entries
    }

    /// How many rows the table holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty. Always false for a constructed table; present
    /// so the type is usable where an emptiness check is idiomatic.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Pick a row, weighted by frequency.
    fn sample(&self, rng: &mut RngStream) -> Option<&ArchetypeEntry> {
        let mut remaining =
            u64::from(rng.next_bounded(u32::try_from(self.total_weight).unwrap_or(u32::MAX)));
        for entry in &self.entries {
            let weight = u64::from(entry.weight);
            if remaining < weight {
                return Some(entry);
            }
            remaining -= weight;
        }
        // Rounding at the top of the range lands here; the last row is the honest
        // choice rather than a panic on an off-by-one.
        self.entries.last()
    }
}

/// Everything a game needs to know about an NPC that is not yet simulated.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NpcBlueprint {
    /// The index this blueprint was generated for. Identities are allocated from
    /// it, so it is stable across processes.
    pub index: u32,
    /// What this person is called.
    pub name: Name,
    /// The naming culture the name came from.
    ///
    /// A blueprint keeps its style so that a collision is broken with an epithet from the
    /// same culture as the name: a Highland called "of the Glen" rather than "of the Fjord".
    pub style: NameStyle,
    /// The archetype row that produced them.
    pub archetype: Archetype,
    /// A short label from the table, owned so a blueprint read from a save file does
    /// not have to borrow from a compile-time table.
    pub label: String,
    /// Their personality.
    pub temperament: Temperament,
    /// Their body and dress, as a seed the game's own content reads.
    pub appearance_seed: u64,
    /// Their starting needs.
    pub needs: crate::domain::temperament::Needs,
    /// Content tags: loadout, dialogue category, faction flavour.
    pub tags: SmallVec<[TagId; MAX_TAGS]>,
    /// A short prose fact a game may use as a conversation hook.
    pub background: String,
}

impl NpcBlueprint {
    /// Break a name collision deterministically.
    ///
    /// Two NPCs can draw the same given and family name — 100,000 draws from a
    /// pool this size produces a handful — and a crowd where three people answer
    /// to "Aldric Vane" reads as a bug. The ordinal comes from the generator's
    /// collision rule (the second Aldric is `Major`, the third `the Younger`), so
    /// the same crowd always disambiguates the same way.
    ///
    /// The epithet comes from this blueprint's own style, so it belongs to the same
    /// culture as the name it is appended to.
    pub fn disambiguate(&mut self, ordinal: u32) {
        if ordinal == 0 || self.name.epithet.is_some() {
            return;
        }
        let epithets = self.style.epithets();
        let pick = usize::try_from(ordinal.saturating_sub(1))
            .unwrap_or(0)
            .wrapping_rem(epithets.len());
        self.name.epithet = epithets.get(pick).map(|value| (*value).to_owned());
    }
}

/// Why a table could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GeneratorError {
    /// No archetype rows were supplied.
    #[error("an archetype table needs at least one row")]
    EmptyTable,
    /// Every row weighed zero, so nothing could ever be drawn.
    #[error("every archetype row weighs zero, so generation could never produce an NPC")]
    ZeroWeight,
}

/// A generator: a seed, a style and a table, ready to produce blueprints.
#[derive(Debug, Clone)]
pub struct Generator {
    seed: SimSeed,
    style: NameStyle,
    table: ArchetypeTable,
}

impl Generator {
    /// Build a generator.
    #[must_use]
    pub const fn new(seed: SimSeed, style: NameStyle, table: ArchetypeTable) -> Self {
        Self { seed, style, table }
    }

    /// Build a generator whose style is chosen by the seed.
    ///
    /// One city has one naming culture; deriving it from the world seed keeps that
    /// true without the game having to decide.
    #[must_use]
    pub fn from_seed(seed: SimSeed, table: ArchetypeTable) -> Self {
        let mut chooser = RngStream::new(seed, u64::MAX);
        let styles = NameStyle::ALL;
        let style = styles
            .get(chooser.next_bounded(num::usize_to_u32(styles.len())) as usize)
            .copied()
            .unwrap_or(NameStyle::Nordic);
        Self { seed, style, table }
    }

    /// The naming culture in use.
    #[must_use]
    pub const fn style(&self) -> NameStyle {
        self.style
    }

    /// The archetype table.
    #[must_use]
    pub const fn table(&self) -> &ArchetypeTable {
        &self.table
    }

    /// Generate one blueprint.
    ///
    /// Pure: the result depends on the seed, the index, the style and the table,
    /// and on nothing else — not on the order other blueprints were generated in.
    #[must_use]
    pub fn generate(&self, index: u32) -> NpcBlueprint {
        // A per-index stream, so blueprint N is the same whether the game generated
        // N-1 first or skipped straight to N.
        let mut rng = RngStream::new(self.seed, u64::from(index) + 1);
        let entry = self.table.sample(&mut rng);
        let (archetype, label, tags) = entry.map_or_else(
            || (Archetype::Wanderer, "traveller".to_owned(), SmallVec::new()),
            |row| (row.archetype, row.label.to_owned(), row.tags.clone()),
        );
        let name = self.style.compose(&mut rng, None);
        // Personality is the archetype's shape with individual variation around it.
        let base = Temperament::archetype(archetype);
        let noise = Temperament::sample(&mut rng);
        let temperament = Temperament {
            openness: blend(base.openness, noise.openness),
            conscientiousness: blend(base.conscientiousness, noise.conscientiousness),
            extraversion: blend(base.extraversion, noise.extraversion),
            agreeableness: blend(base.agreeableness, noise.agreeableness),
            neuroticism: blend(base.neuroticism, noise.neuroticism),
            bravery: blend(base.bravery, noise.bravery),
            sociability: blend(base.sociability, noise.sociability),
            industriousness: blend(base.industriousness, noise.industriousness),
        };
        let appearance_seed = rng.next_u64();
        let backgrounds = BACKGROUNDS;
        let background = backgrounds
            .get(rng.next_bounded(num::usize_to_u32(backgrounds.len())) as usize)
            .copied()
            .map_or_else(
                || "keeps their history to themselves".to_owned(),
                str::to_owned,
            );
        NpcBlueprint {
            index,
            name,
            style: self.style,
            archetype,
            label,
            temperament,
            appearance_seed,
            needs: crate::domain::temperament::Needs::satisfied(),
            tags,
            background,
        }
    }

    /// Generate `count` blueprints, breaking name collisions deterministically.
    ///
    /// The collision rule is per batch, not per blueprint: a name is only
    /// disambiguated if an earlier index in the same batch already claimed it, so
    /// blueprint 7,000 does not depend on whether the caller asked for 7,001.
    /// Batches that overlap therefore agree about the low indices and may differ
    /// about a duplicate that only appears in the larger batch — which is why the
    /// ambient tier generates a whole district at once rather than one NPC at a
    /// time.
    #[must_use]
    pub fn generate_batch(&self, count: u32) -> Vec<NpcBlueprint> {
        let mut seen: Vec<(String, String)> = Vec::with_capacity(count as usize);
        let mut out = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut blueprint = self.generate(index);
            let key = (blueprint.name.given.clone(), blueprint.name.family.clone());
            let ordinal = seen
                .iter()
                .filter(|(given, family)| *given == key.0 && *family == key.1)
                .count();
            if ordinal > 0 {
                blueprint.disambiguate(u32::try_from(ordinal).unwrap_or(u32::MAX));
                if let Some(epithet) = &blueprint.name.epithet {
                    seen.push((
                        blueprint.name.given.clone(),
                        format!("{}{}", blueprint.name.family, epithet),
                    ));
                } else {
                    seen.push(key);
                }
            } else {
                seen.push(key);
            }
            out.push(blueprint);
        }
        out
    }
}

/// Blend an archetype's weight with an individual's sampled weight.
///
/// Two thirds of the archetype, one third the individual: enough variation that
/// two guards are not the same person, enough signal that a guard still reads as
/// a guard.
fn blend(base: Weight, noise: Weight) -> Weight {
    Weight::clamped(noise.get().mul_add(0.34, base.get() * 0.66))
}

/// Prose hooks a game may use as background, or ignore.
const BACKGROUNDS: [&str; 8] = [
    "came in with the grain barges and never left",
    "was apprenticed to a trade they turned out to hate",
    "keeps a debt they do not discuss",
    "sends most of their wages home",
    "was raised two streets over, which explains the accent",
    "has been on the wrong side of the watch once",
    "signed on for one campaign and came back alone",
    "inherited a shop and none of the skill",
];

/// Generate one blueprint from a seed and an index, in the seed's own name style.
#[must_use]
pub fn generate(seed: SimSeed, index: u32, table: &ArchetypeTable) -> NpcBlueprint {
    Generator::from_seed(seed, table.clone()).generate(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{OrDie, OrDieErr};

    fn table() -> ArchetypeTable {
        ArchetypeTable::standard().or_die()
    }

    #[test]
    fn the_same_triple_yields_the_same_blueprint() {
        let a = generate(SimSeed::new(7), 42, &table());
        let b = generate(SimSeed::new(7), 42, &table());
        assert_eq!(a.name, b.name);
        assert_eq!(a.archetype, b.archetype);
        assert_eq!(a.appearance_seed, b.appearance_seed);
        assert_eq!(a.temperament, b.temperament);
    }

    #[test]
    fn generation_order_does_not_matter() {
        let generator = Generator::from_seed(SimSeed::new(99), table());
        let first = generator.generate(5);
        let _ = generator.generate(6);
        let again = generator.generate(5);
        assert_eq!(first.name, again.name);
        assert_eq!(first.appearance_seed, again.appearance_seed);
    }

    #[test]
    fn a_different_seed_produces_a_different_crowd() {
        let one = generate(SimSeed::new(1), 0, &table());
        let other = generate(SimSeed::new(2), 0, &table());
        assert_ne!(one.name, other.name);
    }

    #[test]
    fn twenty_five_thousand_blueprints_rarely_collide() {
        let generator = Generator::from_seed(SimSeed::new(0x5EED), table());
        // Twenty-five thousand is enough to stabilise the rate and cheap enough that
        // an unoptimised debug build does not spend a minute on it. The rate is a
        // property of the pools, not of the sample size.
        let blueprints = generator.generate_batch(25_000);
        let mut keys: Vec<(String, String)> = blueprints
            .iter()
            .map(|b| (b.name.given.clone(), b.name.family.clone()))
            .collect();
        keys.sort();
        keys.dedup();
        let total = 25_000_u32;
        let collisions = total - num::usize_to_u32(keys.len());
        // The pools give roughly 500 onsets x vowels x codas per syllable pair, times
        // 16-18 surnames: a collision rate under 1% is generous, and the point of the
        // test is that the number is measured rather than assumed.
        assert!(
            collisions * 100 <= total,
            "collision rate {}% is above the 1% budget",
            f64::from(collisions) / f64::from(total) * 100.0
        );
    }

    #[test]
    fn batch_generation_is_unique_after_disambiguation() {
        let generator = Generator::from_seed(SimSeed::new(3), table());
        let blueprints = generator.generate_batch(20_000);
        let mut names: Vec<String> = blueprints.iter().map(|b| b.name.full()).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "batch generation left duplicate names");
    }

    /// Every style draws a name, and the pools are never empty.
    ///
    /// An empty pool would make `compose` fall back to its unreachable arm and produce a
    /// nameless NPC, which is a bug the collision tests would not catch.
    #[test]
    fn every_style_draws_a_name_from_its_own_pools() {
        for style in NameStyle::ALL {
            assert!(!style.onsets().is_empty(), "{style} has no onsets");
            assert!(!style.vowels().is_empty(), "{style} has no vowels");
            assert!(!style.codas().is_empty(), "{style} has no codas");
            assert!(!style.surnames().is_empty(), "{style} has no surnames");
            assert!(!style.epithets().is_empty(), "{style} has no epithets");
            let mut rng = RngStream::new(SimSeed::new(21), 7);
            let name = style.compose(&mut rng, None);
            assert!(!name.given.is_empty(), "{style} drew an empty given name");
            assert!(!name.family.is_empty(), "{style} drew an empty family name");
        }
    }

    /// A collision is broken with an epithet from the blueprint's own culture.
    #[test]
    fn disambiguation_uses_the_blueprints_own_style() {
        for style in NameStyle::ALL {
            let mut blueprint = generate(SimSeed::new(8), 3, &table());
            blueprint.style = style;
            blueprint.disambiguate(1);
            let Some(epithet) = blueprint.name.epithet.as_deref() else {
                unreachable!("a blueprint that asked for an epithet was given none");
            };
            assert!(
                style.epithets().contains(&epithet),
                "{style} was disambiguated with `{epithet}`, which is not its own"
            );
        }
    }

    /// A given name has one capital, at its start.
    ///
    /// The onset pools are capitalised because the first one opens the name. The second
    /// syllable's onset sits mid-word, and a capital there reads as a typo: `SkaNaum`
    /// rather than `Skanaur`.
    #[test]
    fn a_given_name_has_one_capital_and_it_is_the_first_letter() {
        for style in NameStyle::ALL {
            let mut rng = RngStream::new(SimSeed::new(31), 3);
            for _ in 0..200 {
                let name = style.compose(&mut rng, None);
                let capitals = name.given.chars().filter(char::is_ascii_uppercase).count();
                assert_eq!(
                    capitals, 1,
                    "{style} drew `{}`, which has {capitals} capitals",
                    name.given
                );
                assert!(
                    name.given.starts_with(char::is_uppercase),
                    "{style} drew `{}`, which does not start with a capital",
                    name.given
                );
            }
        }
    }

    /// The Highland style is in the rotation, and a seed that picks it says so.
    ///
    /// A style nobody can reach is a style that does not exist, and `ALL` is the only
    /// list that decides what a seed can land on.
    #[test]
    fn highland_is_part_of_the_rotation() {
        assert!(
            NameStyle::ALL.contains(&NameStyle::Highland),
            "Highland is not in the rotation"
        );
        assert_eq!(NameStyle::Highland.as_str(), "highland");
        assert_eq!(NameStyle::ALL.len(), 4);
    }

    #[test]
    fn disambiguation_is_deterministic() {
        let mut a = generate(SimSeed::new(4), 1, &table());
        let mut b = generate(SimSeed::new(4), 1, &table());
        a.disambiguate(2);
        b.disambiguate(2);
        assert_eq!(a.name, b.name);
        assert!(a.name.epithet.is_some());
    }

    #[test]
    fn archetype_frequencies_follow_the_weights() {
        let table = table();
        let generator = Generator::from_seed(SimSeed::new(11), table);
        let mut guards = 0;
        let total = 20_000;
        for index in 0..total {
            if generator.generate(index).archetype == Archetype::Guard {
                guards += 1;
            }
        }
        // Guards weigh 12 of 100, so 12% is the expectation; allow two points either way.
        let share = f64::from(guards) / f64::from(total) * 100.0;
        assert!(
            (10.0..=14.0).contains(&share),
            "guard share was {share:.1}%"
        );
    }

    #[test]
    fn an_empty_table_is_rejected() {
        assert_eq!(
            ArchetypeTable::new(Vec::new()).or_die_err(),
            GeneratorError::EmptyTable
        );
        let zero = ArchetypeEntry::new(Archetype::Guard, 0, "nobody");
        assert_eq!(
            ArchetypeTable::new(vec![zero]).or_die_err(),
            GeneratorError::ZeroWeight
        );
    }

    #[test]
    fn appearance_is_a_seed_not_an_asset_path() {
        let blueprint = generate(SimSeed::new(8), 3, &table());
        assert_ne!(blueprint.appearance_seed, 0);
        assert!(blueprint.tags.len() <= MAX_TAGS);
    }

    #[test]
    fn names_render_with_and_without_an_epithet() {
        let mut blueprint = generate(SimSeed::new(8), 3, &table());
        let bare = blueprint.name.full();
        assert_eq!(
            bare,
            format!("{} {}", blueprint.name.given, blueprint.name.family)
        );
        blueprint.disambiguate(1);
        let disambiguated = blueprint.name.full();
        assert!(
            disambiguated.starts_with(&bare),
            "the epithet replaced the name"
        );
        assert!(disambiguated.len() > bare.len(), "no epithet was appended");
        assert!(disambiguated.contains(' '), "the epithet did not render");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_blueprint_survives_a_round_trip() {
        let blueprint = generate(SimSeed::new(12), 9, &table());
        let encoded = ron::to_string(&blueprint).or_die();
        let decoded: NpcBlueprint = ron::from_str(&encoded).or_die();
        assert_eq!(decoded.name, blueprint.name);
        assert_eq!(decoded.appearance_seed, blueprint.appearance_seed);
        assert_eq!(decoded.archetype, blueprint.archetype);
    }
}
