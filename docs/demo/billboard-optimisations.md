# Where the billboard pass can still go faster

The level-of-detail switch in the terminal front end draws a body past
`Shot::impostor_distance` as a two-triangle camera-facing quad and everything nearer as its glTF
mesh. At the interactive budget — 112 by 72 cells, a 24 m threshold, eight metres of cross-fade —
that pass is the cheapest part of the picture: 19.39 ms a frame with the band switched on against
19.63 ms with it off, which is this machine's noise floor. The proposals below are for the case the
band was built for, a 300-cell terminal where a body is eight to ten pixels tall and the mesh
dominates the frame, and each one names how to settle whether it is worth doing.

None of them is measured yet. Each entry says what to measure and what would make it a no.

## 1. Shade the card once, not once per pixel

**What it does now.** `push_billboard` builds two triangles with `Style::Lit` and a normal of
`-forward`, so every pixel of the card runs the general fragment path: a normal dot product against
two lights, a rim term, and a fog evaluation. Every one of those is constant across the card — a
billboard is flat by construction, it faces the camera by construction, and it is one depth.

**Proposal.** Add `Style::Card`, carrying a colour already lit and already fogged. The pass computes
it once per body (one dot product, one fog lookup) and the fragment path becomes "emit this colour".

**Why it should win.** At 112 by 72 the card is about twenty pixels and the per-pixel chain is most of
what a card costs; the pass is dominated by fragment work, not by setup. At 300 cells a card is eighty
pixels, where the ratio gets better still.

**How to settle it.** `cargo bench --bench render_pipeline -- software/interactive` with a third case
that draws the budgeted shot with every body forced past the threshold, so the frame is pure cards.
If the card case does not drop by at least a tenth, the fragment path is not the cost and this is a
no.

**Risk.** Two shading paths for one surface can drift apart. The guard is a test that a card's colour
at the threshold equals the mesh's flat-lit colour at the same distance within a channel unit or two,
which is exactly what `the_band_is_a_ramp_and_not_a_step` already compares.

## 2. Fuse the cards into one pass per cell bucket

**What it does now.** Each body calls `push_billboard`, which projects four corners, clips, and
rasterises two triangles with its own bounds scan. Opaque geometry is order-independent, so at three
pixels a body the per-body setup is all overhead.

**Proposal.** Collect the cards for a cell bucket, emit them as one vertex array with a shared
transform, and submit one relaxed depth pass. Cards never need sorting against each other —
they are opaque and depth-tested — so batching is safe in a way it would not be for a blended fade.

**Why it should win.** The setup cost per body (two clips, one bounds scan, one face construction) is
fixed and the pixel cost is tiny, so this changes the shape of the curve rather than its constant.

**How to settle it.** Measure the pure-card case from proposal 1, then count how much of it is spent
above the fragment path: if the card case is 6 ms and the flattened fragment cost is 3 ms, the other
3 ms is setup and fusion is worth it.

**Risk.** More code paths in a rasteriser is a maintenance cost, and the win only exists below the
size where a body is a smudge. Do it only if the pure-card case is a meaningful share of a 300-cell
frame.

## 3. Align the card to the mesh's own bounds

**What it does now.** The card is 1.75 m tall and 0.26 as wide as it is tall, centred on the body's
origin, for every model. The urchins of the Ashfall Verge are squat and the pylons are not, so the
card and the mesh it replaces disagree about how much of the screen the body occupies. That is the
pop the band is designed to hide, and a card that is the wrong shape makes the band's work harder.

**Proposal.** Derive the card's height, width and vertical offset from the mesh's measured bounds
(`Mesh` already carries a height for exactly this reason) and centre it on the bounds, not the
origin. Then the card is the mesh's silhouette to within the projection error.

**Why it should win.** It does not make anything faster — it makes the threshold cheaper to live
with, which lets the threshold come *nearer*, which is where the frame time actually is. Every metre
the switch comes in is a body's worth of triangles moved off the mesh path.

**How to settle it.** Project the mesh's bounds and the card's bounds at the same distance and
require their screen-space boxes to agree within a pixel; then lower `impostor_distance` until that
test fails and use the value below it.

**Risk.** A per-model card aspect means the billboard code needs the mesh, which the pure-card case
in proposal 1 may want to skip. Keep the fallback: no mesh, no card, use the proportions in the
asset table.

## 4. Cull sub-pixel cards before the setup

The mesh path already has `Shot::min_area` to drop triangles smaller than a fraction of a pixel.
Cards have no equivalent: a body thirty cells away still pays two full triangle setups to fill a
fraction of one pixel. The cheapest win in the list is to reuse the same rule on the card's projected
area, and it is the only one that needs no new code path — one comparison where `push_billboard` is
called.

## 5. Rotate the dither per frame

The band's dither is fixed in screen space, so it crawls as a body moves. A four-by-four rotation
indexed by frame parity would hide it, at the cost of one shift. Only worth doing if the band is ever
turned on for an offline image; the shipped views keep it at zero.

## What not to do

- **Blending.** It is the obvious way to fade, and it is wrong here: a blended fragment cannot write
  depth, so a body stops occluding itself. That was the first implementation, and it drew blotches
  where a hidden facet overpainted a visible one.
- **Sorting the cards.** They are opaque; sorting them is a sort for its own sake.
- **A texture for the card.** There is no sampler in the rasteriser, and adding one to save a few
  kilobytes of geometry would cost more than the geometry does.
