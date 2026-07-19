# Manim cost-model calibration evidence

This file preserves the measurements used to shape `DESIGN.md`. They are calibration evidence for one machine and one evolving Manim fork, not universal wall-time constants for lint diagnostics.

## Snapshot

- Captured: 2026-07-17 (Asia/Tokyo)
- Base commit: `4d25c031ffe7`
- Manim version: `0.20.1`
- OS: WSL2, Linux `6.6.87.2-microsoft-standard-WSL2`, x86-64
- CPU: Intel Core Ultra 7 255H, 16 online logical CPUs
- System Python: CPython 3.12.3
- OpenGL probe used by the source audit: EGL / d3d12, Intel Arc Pro 140T
- Source manifest SHA-256: `a107ef99c811dcc681e353171b97d5ecaf3ddebb520a71554c16b28d5f14d294`

The source manifest was computed with:

```bash
{ find manim -type f -not -path '*/__pycache__/*' -print0; printf '%s\0' pyproject.toml; } \
  | sort -z | xargs -0 sha256sum | sha256sum
```

The worktree contained substantial uncommitted renderer changes. The source digest, not the base commit alone, identifies this calibration snapshot.

## Measurements retained by the cost model

| Stage / workload | Observed measurement | Model implication |
| --- | --- | --- |
| Cairo Transform, 300 members, 60 frames | interpolation `130.658 → 33.004 ms/play`; steady `2.0761 → 0.1890 ms/frame` with packed path | retain `frames × family × channels` even when a gated fast path exists |
| updater-free recursive family | 1,000 nodes `0.1396 → 0.0327 ms/frame`; 3,000 nodes `0.3665 → 0.0335 ms/frame` with sealed elision | an empty recursive walk is still `frames × family` |
| Transform begin, 500 members, matching topology | about `11.436 ms`; copy was 64.6% | model copy separately from interpolation |
| Transform begin, 1→16 curve mismatch | about `64.466 ms`; alignment was 81.5% | topology / curve-count delta is an independent dimension |
| Text, 1,000 characters | vectorized glyph closure saved about `20.9 ms` end to end | Text construction is not a trivial object allocation |
| 16 distinct cold MathTex expressions | sequential `120.724 s`; submit-all/collect `11.717 s` | distinct cold resource keys and external processes must be modeled |
| 7 cold Graph labels | `24.662 → 4.760 s` with local-fork precompile | precompile advice requires multiple distinct keys and the local capability |
| Cairo Surface `(32, 32)`, 1,024 faces | pre-optimization z-sort about `17 ms/frame`; projection about `6.4 ms/frame`; optimized 1080p capture still about `113.6 ms/frame` in the recorded probe | use faces × frames, camera motion, shading, stroke and pixels; `MLP213` starts its advisory face gate at this recorded workload |
| fork-per-play Cairo, 1080p | Bayes `7.55 → 3.95 s`; Algorithm `12.13 → 7.50 s` in the recorded A/B | fork blockers belong in a local-profile cost report, not generic warnings |

Additional end-to-end runs later in the same audit measured different absolute totals as more optimizations and cache states changed. The linter must therefore keep symbolic multiplicities as its primary result and never embed these milliseconds as portable truth.

## Provenance

The source audit documents at capture time had these hashes:

```text
906ee610d197261c878a7c8701558e22a10ac0282366f1365045348a585f56d2  .tmp_perf_audit_execution_log.md
cb90c84e221bed02446aaf2b1ccaeead057859990353c9c2dc7ad359a1307912  .tmp_perf_audit_candidates.md
e3f8e69a3f33b5c92870eb75160aaa6f6fdd3beb70d07f437dc7696e2a950edf  .tmp_perf_audit_deep_dive.md
```

The detailed A/B protocols, correctness hashes, cache conditions and reproducer names were recorded there. If a future implementation imports coefficients from benchmark output, it must store the raw result, command, source digest, renderer, resolution, FPS, cache state and machine profile in a versioned calibration JSON rather than referring to a temporary file.
