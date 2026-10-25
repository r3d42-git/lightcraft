# What's new in LightCraft

## 2026-10-25


### The wgpu window requests a shorter presentation queue
- Preserve eframe's low-latency surface (PR #2919): request one queued frame instead of two.
  This can reduce present-to-composite delay; glow fallback is unaffected. No native window
  latency measurement has been made for this fork. Rendering, edits and cache versions are unchanged.
