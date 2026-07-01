# change.md

- **change_id**: Auth-CI-floor-cookbook
- **status**: implementing
- **created**: 2026-06-30
- **updated**: 2026-07-01
- **description**: CI floor + auth guard — wire fmt-check, slow_tests, and grep-based constant-time-compare guard into ci.yml and deploy.yml via Makefile. Make constant_time_eq_str pub(crate) + direct unit test. First of 3 decomposed changes from Phase 4.
- **origin**: test-plan.md §3 Phase 4 row
- **scope**: CI floor + auth guard only (Risk #7). Coverage threshold and cookbook backfill are separate future changes.
