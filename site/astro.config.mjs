// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// The site is the adoption path: install, first crowd, concepts, guides. The tuning material -- ADRs, performance
// journals, gates, benchmark harnesses -- deliberately stays in the repository under docs/dev and is linked, not
// republished, so there is one copy of every number.
export default defineConfig({
  site: 'https://greysquirr3l.github.io/bevy-flock/',
  integrations: [
    starlight({
      title: 'bevy-flock',
      description: 'A deterministic NPC and crowd engine for Bevy: 10k agents in a frame, a million when you need them.',
      logo: { src: './src/components/Logo.astro' },
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/greysquirr3l/bevy-flock' }],
      customCss: ['./src/styles/custom.css'],
      editLink: { baseUrl: 'https://github.com/greysquirr3l/bevy-flock/edit/main/site/' },
      sidebar: [
        { label: 'Start here', items: [
          { label: 'Install', slug: 'start/getting-started' },
          { label: 'A crowd in five lines', slug: 'start/first-crowd' },
          { label: 'Concepts', slug: 'start/concepts' },
          { label: 'What is in the box', slug: 'start/whats-in-the-box' },
        ]},
        { label: 'Guides', items: [
          { label: 'Behaviour that varies', slug: 'guides/behaviour' },
          { label: 'Memory and grudges', slug: 'guides/memory' },
          { label: 'Saving a crowd', slug: 'guides/saving' },
          { label: 'Scaling to a million', slug: 'guides/scaling' },
        ]},
        { label: 'Reference', items: [
          { label: 'Feature flags', slug: 'reference/features' },
          { label: 'Performance', slug: 'reference/performance' },
          { label: 'Feature coverage', slug: 'reference/coverage' },
          { label: 'Where the internals live', slug: 'reference/internals' },
        ]},
      ],
    }),
  ],
});
