# Config Format Upgrade: Multi-Format + External Patterns

- **created**: 2026-06-11
- **updated**: 2026-06-13
- **last_research**: config-format (2026-06-11)
- **archived_at**: 2026-06-13T08:39:27Z

- **status**: archived
- **summary**: Upgrade Cerebrum's configuration system to support both YAML and TOML formats (via serde derives) and externalize regex patterns into pattern files. This improves UX for non-Rust engineers and eliminates regex escaping issues. Fully backward compatible with existing config.toml.

- **review**: reviews/impl-review.md (APPROVED; 171 tests pass; manual integration tests 51/51 passed)