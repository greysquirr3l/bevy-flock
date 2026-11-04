# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| 0.1.x | Yes |

## Reporting a vulnerability

Open a private security advisory on GitHub rather than a public issue:

<https://github.com/greysquirr3l/bevy-flock/security/advisories/new>

Include the version, a minimal reproduction, and what an attacker gains.

## Threat model

This crate simulates NPCs. It does not parse untrusted input, open sockets, touch
the filesystem, or execute anything a data file asks it to. The realistic
security surface is therefore small, and it is worth being precise about it:

- **Resource exhaustion.** A game that feeds a population count from untrusted
  data can be made to allocate. `PopulationSize::try_new` rejects zero and caps at
  `MAX_POPULATION`, and spawning is budgeted, but the cap is a
  denial-of-service ceiling, not a security boundary. Validate before you trust.
- **NaN propagation.** A NaN position from a physics or gameplay system can
  propagate through steering into every neighbour that reads it. Steering
  sanitises its own outputs and the public constructors reject non-finite
  configuration, but a NaN injected into a `Transform` by other code is the
  integrator's responsibility.
- **`unsafe`.** There is none: `#![deny(unsafe_code)]` is a crate-level
  invariant, checked in CI.

## Determinism is not a security property

The determinism guarantee assumes the same binary and feature set. Do not use it
as an anti-cheat mechanism: a client that controls its own binary controls its
own simulation. Server-authoritative design is a separate problem.
