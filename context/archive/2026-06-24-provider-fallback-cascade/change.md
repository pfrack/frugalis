# Change: Provider Fallback / Cascade

- **id:** provider-fallback-cascade
- **status:** archived
- **created:** 2026-06-24
- **updated:** 2026-06-26
- **archived_at:** 2026-06-26T21:53:02Z
- **roadmap_id:** S-17
- **description:** When an upstream provider fails (5xx, timeout, rate-limit), automatically retry on the next configured provider in priority order. Each routing category defines an ordered list of providers; first healthy one wins.
