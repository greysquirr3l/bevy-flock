//! Navigation links: ladders, ledges, doors and anything a grid cannot say.
//!
//! A grid of cells describes ground you can walk on. It cannot describe a ladder that
//! costs four seconds, a ledge that is a shortcut, or a door that only opens for staff.
//! Those are *links*: a pair of cells that are adjacent in the world's story rather than
//! in its geometry, with a cost and an optional tag.
//!
//! The table is sorted arrays, not a map. That is deliberate: a search iterates it in a
//! fixed order, and a hash map's iteration order (and therefore the tie-break between two
//! equally cheap routes through two different ladders) is not something a replay can rely
//! on (ADR 0002).
//!
//! Two indices exist because a flow field is solved *backwards* from its target: the
//! expansion asks "what arrives here", not "what leaves here". Building a field that
//! ignored that would follow every ladder in the wrong direction, which is a mistake worth
//! a test rather than a comment.
//!
//! Tags are how a behaviour tree learns that an agent's route uses the ladder rather than
//! the stairs: [`crate::domain::flow::FlowField::link_tag_at`] reports the tag of the link a
//! cell was reached through, which is a fact a tree can branch on without a custom node.

use bevy_math::Vec3;
use smallvec::SmallVec;

use crate::domain::nav::{NavCell, NavError, NavSource};

/// The tag of a cell an agent did not reach through a link.
pub const UNTAGGED: LinkTag = LinkTag(0);

/// What kind of traversal a link is, as a small integer a game assigns meaning to.
///
/// The crate does not know what a ladder is; it knows that two places are connected at a
/// cost and that the connection has a name. A game decides that `LinkTag(1)` is a ladder
/// and plays the climb animation when it sees one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LinkTag(pub u16);

impl LinkTag {
    /// An unnamed link.
    pub const UNTAGGED: Self = UNTAGGED;

    /// A tag from a number.
    #[must_use]
    pub const fn new(tag: u16) -> Self {
        Self(tag)
    }
}

/// A connection between two cells that is not an ordinary step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NavLink {
    /// The cell the link leaves from.
    pub from: NavCell,
    /// The cell the link arrives at.
    pub to: NavCell,
    /// The integer cost of taking it, in the same units as a step
    /// ([`crate::domain::nav::COST_ORTHOGONAL`] is 10, so a four-second climb against a
    /// 0.26 s step is about 150).
    pub cost: u32,
    /// What kind of traversal it is, for animation and for behaviour.
    pub tag: LinkTag,
    /// Whether the link can be taken the other way.
    pub bidirectional: bool,
}

impl NavLink {
    /// A one-way link.
    #[must_use]
    pub const fn new(from: NavCell, to: NavCell, cost: u32, tag: LinkTag) -> Self {
        Self {
            from,
            to,
            cost,
            tag,
            bidirectional: false,
        }
    }

    /// A link that can be taken either way.
    #[must_use]
    pub const fn two_way(from: NavCell, to: NavCell, cost: u32, tag: LinkTag) -> Self {
        Self {
            from,
            to,
            cost,
            tag,
            bidirectional: true,
        }
    }

    /// A packed key for the `from` cell, so the table can be sorted without knowing the
    /// grid width.
    const fn key(self) -> u32 {
        key_of(self.from)
    }

    /// A packed key for the `to` cell.
    const fn to_key(self) -> u32 {
        key_of(self.to)
    }
}

/// Pack a cell into a sortable key: row in the high half, column in the low half.
const fn key_of(cell: NavCell) -> u32 {
    (cell.y as u32) << 16 | cell.x as u32
}

/// Insert a directed edge into an array sorted by `key`, using `secondary` for the tie.
fn insert_sorted(
    edges: &mut Vec<NavLink>,
    edge: NavLink,
    key: impl Fn(&NavLink) -> u32,
    secondary: impl Fn(&NavLink) -> u32,
) {
    // Strictly-less, so `at` is the *first* edge with this key and the tie-break below has a
    // slice of same-key edges to work in. Partitioning on `<=` would land past all of them and
    // leave insertion order deciding ties.
    let at = edges.partition_point(|existing| key(existing) < key(&edge));
    let offset = edges
        .get(at..)
        .unwrap_or_default()
        .iter()
        .take_while(|existing| key(existing) == key(&edge))
        .take_while(|existing| secondary(existing) < secondary(&edge))
        .count();
    edges.insert(at + offset, edge);
}

/// Links, sorted for a fixed iteration order, indexed both ways.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LinkTable {
    /// Every traversable direction, sorted by `from` cell then by `to` cell.
    leaving: Vec<NavLink>,
    /// The same edges, sorted by `to` cell then by `from` cell, for a backward search.
    arriving: Vec<NavLink>,
    /// How many links were authored, counting a two-way link once.
    authored: usize,
}

impl LinkTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many links the table holds, counting each authored link once.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.authored
    }

    /// Whether the table is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.authored == 0
    }

    /// Add a link.
    ///
    /// # Errors
    ///
    /// [`NavError::BadLink`] when the two ends are the same cell, when the cost is zero
    /// (a free link makes a route free, and a search that finds a free cycle is a search
    /// that does not terminate in spirit) or when the same directed link already exists.
    pub fn add(&mut self, link: NavLink) -> Result<(), NavError> {
        self.validate(&link)?;
        self.push(link);
        if link.bidirectional {
            self.push(NavLink {
                from: link.to,
                to: link.from,
                ..link
            });
        }
        self.authored += 1;
        Ok(())
    }

    fn validate(&self, link: &NavLink) -> Result<(), NavError> {
        let refuse = |reason: &'static str| NavError::BadLink {
            from: link.from,
            to: link.to,
            reason,
        };
        if link.from == link.to {
            return Err(refuse("a link must join two different cells"));
        }
        if link.cost == 0 {
            return Err(refuse("a link cost must be greater than zero"));
        }
        let duplicate = self.leaving.iter().any(|existing| {
            existing.from == link.from && existing.to == link.to && existing.tag == link.tag
        });
        if duplicate {
            return Err(refuse("that link already exists"));
        }
        Ok(())
    }

    fn push(&mut self, edge: NavLink) {
        insert_sorted(
            &mut self.leaving,
            edge,
            |link| link.key(),
            |link| link.to_key(),
        );
        insert_sorted(
            &mut self.arriving,
            edge,
            |link| link.to_key(),
            |link| link.key(),
        );
    }

    /// Append the links leaving a cell: what a walker may take from here.
    pub fn leaving_into(&self, cell: NavCell, out: &mut SmallVec<[(NavCell, u32, LinkTag); 4]>) {
        out.clear();
        let key = key_of(cell);
        let start = self.leaving.partition_point(|link| link.key() < key);
        for link in self
            .leaving
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take_while(|link| link.key() == key)
        {
            out.push((link.to, link.cost, link.tag));
        }
    }

    /// Append the links arriving at a cell: what a backward search may step back across.
    pub fn arriving_into(&self, cell: NavCell, out: &mut SmallVec<[(NavCell, u32, LinkTag); 4]>) {
        out.clear();
        let key = key_of(cell);
        let start = self.arriving.partition_point(|link| link.to_key() < key);
        for link in self
            .arriving
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take_while(|link| link.to_key() == key)
        {
            out.push((link.from, link.cost, link.tag));
        }
    }

    /// Every edge, in table order. A two-way link appears twice.
    pub fn iter(&self) -> impl Iterator<Item = &NavLink> {
        self.leaving.iter()
    }

    /// The tags in use, sorted and deduplicated.
    #[must_use]
    pub fn tags(&self) -> Vec<LinkTag> {
        let mut tags: Vec<LinkTag> = self.leaving.iter().map(|link| link.tag).collect();
        tags.sort_unstable();
        tags.dedup();
        tags
    }

    /// Where a link lands, for an animation hook or a debug overlay.
    #[must_use]
    pub fn landing_point(link: &NavLink, source: &dyn NavSource) -> Vec3 {
        source.center_of(link.to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::nav::GridNavSource;
    use crate::testkit::{GetOrDie, OrDie, OrDieErr};

    fn cell(x: u16, y: u16) -> NavCell {
        NavCell::new(x, y)
    }

    #[test]
    fn a_link_is_found_from_its_from_cell() {
        let mut table = LinkTable::new();
        table
            .add(NavLink::new(cell(2, 2), cell(2, 6), 150, LinkTag::new(1)))
            .or_die();
        let mut out = SmallVec::new();
        table.leaving_into(cell(2, 2), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out.at(0).0, cell(2, 6));
        assert_eq!(out.at(0).1, 150);
        assert_eq!(out.at(0).2, LinkTag::new(1));
    }

    #[test]
    fn a_link_is_found_from_its_to_cell_backwards() {
        let mut table = LinkTable::new();
        table
            .add(NavLink::new(cell(1, 1), cell(5, 2), 20, UNTAGGED))
            .or_die();
        let mut out = SmallVec::new();
        table.arriving_into(cell(5, 2), &mut out);
        assert_eq!(out.len(), 1, "the backward index is missing the edge");
        assert_eq!(out.at(0).0, cell(1, 1));
        table.arriving_into(cell(1, 1), &mut out);
        assert!(out.is_empty(), "a one-way link cannot be walked back");
    }

    #[test]
    fn a_two_way_link_is_traversable_from_both_ends() {
        let mut table = LinkTable::new();
        table
            .add(NavLink::two_way(cell(1, 1), cell(5, 5), 20, UNTAGGED))
            .or_die();
        let mut out = SmallVec::new();
        table.leaving_into(cell(1, 1), &mut out);
        assert_eq!(out.len(), 1);
        table.leaving_into(cell(5, 5), &mut out);
        assert_eq!(out.len(), 1, "the reverse direction is missing");
        table.arriving_into(cell(1, 1), &mut out);
        assert_eq!(out.len(), 1, "a two-way link cannot be walked back");
        assert_eq!(table.len(), 1, "a two-way link is one link, not two");
    }

    #[test]
    fn a_degenerate_link_is_refused() {
        let mut table = LinkTable::new();
        assert!(matches!(
            table
                .add(NavLink::new(cell(1, 1), cell(1, 1), 10, UNTAGGED))
                .or_die_err(),
            NavError::BadLink {
                from: _,
                to: _,
                reason: _
            }
        ));
        assert!(matches!(
            table
                .add(NavLink::new(cell(1, 1), cell(2, 1), 0, UNTAGGED))
                .or_die_err(),
            NavError::BadLink {
                from: _,
                to: _,
                reason: _
            }
        ));
    }

    #[test]
    fn a_duplicate_link_is_refused() {
        let mut table = LinkTable::new();
        let link = NavLink::new(cell(0, 0), cell(0, 4), 10, UNTAGGED);
        table.add(link).or_die();
        assert!(matches!(
            table.add(link).or_die_err(),
            NavError::BadLink {
                from: _,
                to: _,
                reason: _
            }
        ));
    }

    #[test]
    fn iteration_order_does_not_depend_on_insertion_order() {
        let links = [
            NavLink::new(cell(3, 1), cell(3, 9), 10, LinkTag::new(2)),
            NavLink::new(cell(1, 1), cell(1, 9), 10, LinkTag::new(1)),
            NavLink::new(cell(1, 1), cell(4, 9), 30, LinkTag::new(3)),
        ];
        let build = |order: [usize; 3]| {
            let mut table = LinkTable::new();
            for index in order {
                table.add(links.at(index)).or_die();
            }
            table.iter().copied().collect::<Vec<_>>()
        };
        assert_eq!(build([0, 1, 2]), build([2, 1, 0]));
        let sorted = build([1, 0, 2]);
        assert_eq!(sorted.at(0).from, cell(1, 1));
        assert_eq!(sorted.at(1).from, cell(1, 1));
        assert!(
            (sorted.at(1).to.y, sorted.at(1).to.x) > (sorted.at(0).to.y, sorted.at(0).to.x),
            "destinations unsorted: {:?} then {:?}",
            sorted.at(0).to,
            sorted.at(1).to
        );
    }

    #[test]
    fn tags_are_reported_once_each_in_order() {
        let mut table = LinkTable::new();
        table
            .add(NavLink::new(cell(0, 0), cell(0, 2), 10, LinkTag::new(3)))
            .or_die();
        table
            .add(NavLink::new(cell(0, 0), cell(2, 0), 10, LinkTag::new(1)))
            .or_die();
        table
            .add(NavLink::new(cell(1, 0), cell(1, 2), 10, LinkTag::new(3)))
            .or_die();
        assert_eq!(table.tags(), vec![LinkTag::new(1), LinkTag::new(3)]);
    }

    #[test]
    fn a_landing_point_is_the_cell_centre() {
        let grid = GridNavSource::open(Vec3::ZERO, 8, 8, 1.0).or_die();
        let link = NavLink::new(cell(1, 1), cell(3, 4), 10, UNTAGGED);
        assert_eq!(
            LinkTable::landing_point(&link, &grid),
            Vec3::new(3.5, 0.0, 4.5)
        );
    }
}
