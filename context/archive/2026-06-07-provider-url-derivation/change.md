---
change_id: provider-url-derivation
title: Provider URL derivation
status: research-only
created: 2026-06-07
updated: 2026-06-07
archived_at: 2026-06-07
---

## Notes

Research-only. No code changes. The research explored mapping `provider_type` to API path suffixes via a `provider_path()` function. Decision: not worth adding a new function that nothing calls, nor worth adding `base_url`/`endpoint` config complexity at cerebrum's current scale. The research document captures the findings for future reference if/when multi-provider routing justifies it.
