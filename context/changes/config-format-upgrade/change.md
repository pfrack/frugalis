# Config Format Upgrade: Multi-Format + External Patterns

- **created**: 2026-06-11
- **updated**: 2026-06-11
- **last_research**: config-format (2026-06-11)

- **status**: implementing
- **summary**: Upgrade Cerebrum's configuration system to support both YAML and TOML formats (via serde derives) and externalize regex patterns into pattern files. This improves UX for non-Rust engineers and eliminates regex escaping issues. Fully backward compatible with existing config.toml.