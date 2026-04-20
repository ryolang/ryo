# Full-Tree Consistency Sweep Report

**Date:** 2026-04-20
**Scope:** All files under `docs/` after the 6-pass spec coherence plan
**Plan reference:** `docs/superpowers/plans/2026-04-20-ryo-spec-coherence-fixes.md`

---

## Executive Summary

The six-pass coherence plan achieved its primary goals: the `while` keyword replacement is complete across all spec and companion docs, milestone ordering is correct (M20 before M21-M23, M15.5 exists after M15), AI-era perishable claims are removed from Section 1.2, and the Ownership Lite cost clarification is in place. However, the sweep found **4 actionable issues** — two residual "competitive performance" strings in the spec, one Rule 5 violation in `language_comparison.md`, and one stale named-parameters entry in the roadmap summary. Additionally, several pre-existing inconsistencies (not introduced by this plan) are noted for future work.

---

## Scope

### What Was Checked

- **Category A (Performance):** Grepped for "competitive performance" (should be gone), "native performance" (should exist), "~5-10%" (acceptable in context).
- **Category B (Ownership Lite):** Grepped for "no lifetime" claims; verified Section 5.6 ceremony parity and Section 5.8 trade-off paragraph.
- **Category C (AI-era):** Grepped spec Section 1.2 for "2026" or "majority" claims (should be gone).
- **Category D (Loop keywords):** Grepped for "for condition:", "no while", "three forms" across all docs, examples, and `.ryo` files.
- **Category E (Milestones):** Verified M20 ordering in Phase 3 and M15.5 existence in roadmap.
- **Cross-reference:** Feature Availability table vs. "Planned for vX" banners and roadmap milestones.
- **design_issues.md:** Checked for issues resolved by this plan.

### Files Examined

All `.md` and `.ryo` files under `docs/`, both `CLAUDE.md` files, and the `docs/dev/implementation_roadmap.md`.

---

## Issues Found

### Issue 1: "competitive performance" remains in specification.md (Category A)

Two instances were NOT updated to "native performance" during Pass 2:

- **`docs/specification.md:63`** — Section 1 vision paragraph: "...while maintaining memory safety and **competitive performance**."
- **`docs/specification.md:3165`** — Section 14.5 concurrency model: "...while maintaining memory safety and **competitive performance**."

**Expected:** Both should read "native performance" to match the Core Goals bullet (line 70) and `index.md`.

**Severity:** Medium — contradicts the explicit rename performed in Pass 2.

### Issue 2: Rule 5 violation in language_comparison.md (Category B)

- **`docs/language_comparison.md:118`** — Ryo example shows `fn process_data(data: &str) -> &str:` which violates Rule 5 (functions cannot return borrows). Also uses `&input` at call site (line 124), violating Rule 2 (implicit borrows).

**Expected:** The example should demonstrate Ryo's ownership model correctly. Suggested fix: return `str` (owned), remove `&` from parameter and call site.

**Severity:** High — this is in a user-facing comparison document and directly contradicts the ownership rules.

### Issue 3: Stale named-parameters entry in roadmap summary (Category E)

- **`docs/dev/implementation_roadmap.md:2680`** — "What v0.1.0 does NOT include" lists `Named parameters — #[named] (v0.3)`.

This contradicts:
- The Feature Availability table (`docs/specification.md:94`) which lists named parameters as v0.1.
- Milestone 8.5 in the roadmap (line 738) which places named parameters in Phase 2 (part of v0.1).
- The spec Section 6.1.1 which fully documents the feature.

**Expected:** Remove the named parameters line from the "does NOT include" list, or add it to the "What v0.1.0 includes" list.

**Severity:** Medium — internal roadmap contradicts the spec's Feature Availability table.

### Issue 4: Named parameters listed as "Future Extension" in spec Section 19

- **`docs/specification.md:3525`** — Section 19 "Planned Future Extensions" lists "Named Parameters & Default Values" despite the Feature Availability table (line 94) assigning them to v0.1.

**Expected:** Remove from Section 19 "Planned Future Extensions" or move to a "v0.1 features" section since they are already specified in Section 6.1.1.

**Severity:** Low — internal spec inconsistency, but Section 6.1.1 is fully written so the design is clear.

---

## Pre-Existing Issues (Not Introduced by This Plan)

These were not in scope for the 6-pass plan but are noted for completeness:

1. **`&str` parameter syntax in roadmap** — Multiple roadmap milestones (M14, M21, M24, M25) use explicit `&str` for function parameters. Under Rule 2 (implicit borrows), these should be `str`. Affects ~12 function signatures in the roadmap.

2. **`-> &str` return types in roadmap** — `docs/dev/implementation_roadmap.md:1586` (`first_word`) and line 1741 (`trim`) return `&str`, violating Rule 5.

3. **`-> &T` in proposals.md** — `docs/dev/proposals.md:154` shows `fn get(&self) -> &T:` which violates Rule 5.

4. **Performance table in language_comparison.md** — Line 367 claims "Very Fast" CPU performance for Ryo, positioned equal to Rust. The spec's honest positioning (line 70) says "comparable to Go" with ~5-10% DX overhead. "Fast" would be more accurate than "Very Fast".

---

## Design Issues Status

### Resolved by This Plan

| Issue | Status | Resolution |
|-------|--------|------------|
| **Logic Paradoxes (Roadmap Breakers)** | RESOLVED | M20 moved before M21-M23; M4.5 split with capture analysis deferred to M15.5. Marked resolved in design_issues.md. |

### Not Resolved (Correctly Remain Open)

| Issue | Status | Notes |
|-------|--------|-------|
| **#2 Hardcoded Generics Trap** | Open | Not in scope for this plan |
| **#3 Error Handling Overhead** | Open | The "Competitive Performance" label was renamed to "Native Performance" (in the Core Goals bullet), but the underlying tracing overhead concern remains valid |
| **#4 Circular Dependencies** | Open | Not in scope |
| **#5 Specification Holes** | Open | Some items partially addressed (named params now in spec) but `never` type, `main` return, and inherent `impl` blocks remain |
| **#6 Conflicting `!` Operator** | Open | Not in scope |
| **Grey Areas #7-#15** | Open | Not in scope |

### Checklist Items in design_issues.md

No additional checklist items can be checked off by this plan. The six checked items were already marked before this plan began.

---

## Recommendations

1. **Fix Issues 1-4 before merging this branch.** Issues 1 and 2 are direct omissions from the plan's Pass 2 and Pass 3 work. Issues 3-4 are newly discovered contradictions.
2. **File a follow-up issue for pre-existing `&str` syntax in the roadmap.** This is a systematic inconsistency affecting ~15 function signatures and should be addressed in a dedicated pass.
3. **File a follow-up issue for the performance table in language_comparison.md.** The "Very Fast" claim needs recalibration.

---

## Final Status

**Issues Found -- requires follow-up**

Four actionable issues were found. Issues 1-4 should be resolved before the branch is considered complete.
