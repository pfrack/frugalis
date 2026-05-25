---
starter_id: axum
package_manager: cargo
project_name: cerebrum
hints:
  language_family: rust
  team_size: solo
  deployment_target: self-host
  ci_provider: github-actions
  ci_default_flow: auto-deploy-on-merge
  bootstrapper_confidence: first-class
  path_taken: standard
  quality_override: false
  self_check_answers: null
  has_auth: false
  has_payments: false
  has_realtime: true
  has_ai: true
  has_background_jobs: false
---

## Why this stack

This project is an API-first gateway with a short solo timeline, so the Rust API default is the fastest fit with low decision overhead. Axum matches the product shape directly, keeps the stack strongly typed and convention-friendly, and supports the two key workload signals from your requirements: AI-oriented routing logic and continuous response delivery behavior. You selected the quick path, so we kept deployment and CI choices simple and aligned with the starter defaults: self-host plus GitHub Actions with automatic deployment on merge. Scaffolding confidence is first-class, which means setup is expected to be mostly smooth with occasional manual touches rather than high-friction bootstrap work.
