---
change_id: intent-classifier-trait
title: Intent classifier trait
status: impl_reviewed
created: 2026-06-06
updated: 2026-06-07
archived_at: null
---

## Notes

This change introduced the `IntentClassify` trait and `ClassifierChain` for pluggable classification backends. The implementation was delivered as part of the upstream proxy routing sequence (`proxy-intent-routing` and `provider-agnostic-config`). The trait enables future extensions (e.g., ONNX or LLM-based classifiers) without modifying the core handler.

The code is present in `src/intent_classifier.rs` (lines 76-143, 110-124, and associated tests).