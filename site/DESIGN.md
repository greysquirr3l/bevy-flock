# DESIGN.md

> Status: **provisional, agent-supplied direction.** This file was written by the agent, not the owner, because the site
> had no direction and shipping without one is what antislop R-37 forbids. It is a draft of an identity, not a claim to
> have one. Replace any field and the theme follows, because every value below maps to a single CSS token.

## Identity

A field notebook kept by someone who measures things. Not a startup landing page and not a terminal cosplay: paper, ink,
one highlighter colour, and a lot of numbers that were actually measured.

Two consequences, both deliberate:

- The site is a **reading** surface. Long paragraphs, tables with real values, no cards competing for attention.
- The numbers are the personality. Where a marketing site would put a testimonial, this one puts a ratio.

## Personality

Patient, precise, unimpressed. It does not say "blazing fast"; it says 3.78 ms on a shared vCPU and shows the fixture.
It assumes the reader is a developer who will leave if they catch it exaggerating.

## Palette

| role | light | dark | purpose |
| --- | --- | --- | --- |
| page | `#fbf9f4` warm paper | `#12141a` ink | reading surface, not pure white or pure black |
| text | `#1c1b18` | `#e8e4da` | 15:1 and 13:1 against the page |
| muted | `#56524a` | `#a6a29a` | 6.4:1 and 6.1:1, still AA at body size |
| accent | `#8a4b12` amber ink | `#e0a54a` | links, active nav, the one emphasised number |

Amber appears on links and active states only. It is not a background, not a border on every card, and not a glow.

## Typography

| role | stack | reason |
| --- | --- | --- |
| headings | Iowan Old Style, Palatino, Book Antiqua, Georgia, serif | documentation is read like a book; a serif at display sizes separates a page's sections without colour or rules, and these faces ship with the OS so there is no webfont request or layout shift |
| body | system-ui, Segoe UI, Roboto, sans-serif | code-adjacent prose reads better in a sans at 16px, and the system stack keeps the page dependency-free |
| code | ui-monospace, JetBrains Mono, SFMono-Regular | tabular figures matter in the performance tables, and a mono that ships locally avoids a network hop for the one thing on the page that must not reflow |

No webfonts. This is a docs site for a library with no dependencies in its core; loading two font files to say so would be
an odd way to open.

## Layout

Measure capped near 68 characters. Tables are allowed to be wide and scroll horizontally on narrow screens rather than
squeeze. Sections are separated by a hairline rule, not by alternating background colours.

## Mood and dials

- ENERGY 2 of 5: quiet. A reference site that shouts is a reference site nobody reads.
- RHYTHM 2 of 5: page composition follows content. Guides get numbered steps, reference gets tables, the landing page gets
  prose with one comparison table rather than a card grid.
- MOTION 1 of 5: no entrance animation. Starlight's own transitions only.

## What this direction forbids here

Card grids of equal cards, gradient backgrounds, icons used as decoration, arrows on every button, and any number without
a fixture.
