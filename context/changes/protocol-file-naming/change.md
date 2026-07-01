# protocol-file-naming

- **created**: 2026-07-01
- **updated**: 2026-07-01
- **status**: preparing

## Summary

The file names in `src/protocol/` are misleading: they imply protocol data models
but contain exclusively translation functions. The `response`/`responses` singular/plural
homograph causes confusion about which file handles which API. The `mod.rs` has no
documentation explaining the taxonomy.
