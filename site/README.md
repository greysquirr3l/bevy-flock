# bevy-flock docs site

Astro + Starlight, published at <https://greysquirr3l.github.io/bevy-flock/>.

## Local development

```sh
cd site
pnpm install
pnpm dev          # http://localhost:4321/bevy-flock/
```

`npm install && npm run dev` works too; the build is identical.

## Building

```sh
pnpm build        # static output in site/dist/
pnpm preview      # serve dist/ locally
```

## Deploying

```sh
pnpm run deploy:dry   # build, rsync into the Pages clone, stage, do not commit or push
pnpm run deploy       # same, then push and poll until the live page matches the build
```

Deploy settings live in `package.json` under `deploy-docs`: the Pages repo path, subdirectory, and
live URL. `deploy:dry` leaves the publication staged in the Pages clone; a later normal deploy
commits and pushes it. Live verification compares the served page with the local build, so a cached
page from the previous publication does not count as success. Run its regression checks with
`bash scripts/deploy-docs.test.sh`.

If the live URL answers with GitHub's "Site not found" after a deploy, the Pages source is still set
to "Deploy from a branch" on the wrong branch. That is a one-time repository setting:
**Settings → Pages → Source**. Don't toggle it from the script; it is a permission change, not a
build step.

If the site loads but the styling is wrong, the `_astro/` assets are not being served, check that
`.nojekyll` exists in the pages clone; the deploy script writes it on first run.

## What belongs here

Anything that tells a *user* what to do: install, first crowd, concepts, guides, reference.
Anything that tells a *maintainer* why the code is the way it is belongs in `../docs/dev/`, and is
linked rather than republished so there is one copy of every number.
