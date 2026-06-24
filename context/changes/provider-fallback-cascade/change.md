# Change: Provider Fallback / Cascade

- **id:** provider-fallback-cascade
- **status:** implemented
- **created:** 2026-06-24
- **updated:** 2026-06-24
- **roadmap_id:** S-17
- **description:** When an upstream provider fails (5xx, timeout, rate-limit), automatically retry on the next configured provider in priority order. Each routing category defines an ordered list of providers; first healthy one wins.
