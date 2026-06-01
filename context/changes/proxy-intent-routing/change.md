---
id: proxy-intent-routing
status: implementing
created: 2026-06-01
updated: 2026-06-01
user: pfrack
tags: [intent-classification, onnx, proxy-routing, intent-classificator, regex-classifier]
---
# proxy-intent-routing

## What
Add intent classification and upstream model routing to the proxy gateway. Research phase: evaluate ONNX-based local inference via an `intent_classificator` crate as an alternative to the original plan's regex + OpenRouter API fallback.

## Why
S-01 is the north star slice — it's the core product hypothesis. The scaffolding (auth, data layer, dashboard) is complete. The `completion_handler` is a stub waiting for real classification and routing logic.

## Open Questions
- Should fallback classification use ONNX (local, zero-cost per-request) or OpenRouter API (simpler, but per-request cost)?
- Which ONNX runtime crate (tract vs ort)?
- Which classification model (zero-shot NLI vs fine-tuned)?
- How does the `intent_classificator` crate boundary look?
