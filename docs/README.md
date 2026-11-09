# Documentation

Three audiences, three places. This file is the map.

| you are | read | ships to crates.io |
| --- | --- | --- |
| adopting the crate | `site/` (published), then `docs/book/` | no |
| changing the engine | `docs/dev/` | no |
| looking at the demo | `docs/demo/` | no |

## site/

Astro + Starlight, published at <https://greysquirr3l.github.io/bevy-flock/>. Install, first crowd, concepts, guides and
reference. `site/DESIGN.md` holds the design direction; `site/README.md` holds the build and deploy steps.

## book/

The long-form guide, built with mdBook. Older and deeper than the site, and it predates it. Where they disagree, the site
is newer and the code is newer still.

## dev/

Everything a maintainer reads while tuning:

```text
dev/
├── adr/      architecture decision records, indexed, 0001 to 0060
├── lint/     the lint policy and the stable-lint drift ledger
├── meta/     1.0 readiness and both release checklists
└── tuning/   performance journals, memory, gates, benchmarks, research, playbooks, data
```

Nothing in this directory is packaged: both manifests exclude `docs/`, so a consumer downloading the crate gets code,
examples and the README. The rule is written down here because "why is the tarball small" is a question that comes back
every release.
