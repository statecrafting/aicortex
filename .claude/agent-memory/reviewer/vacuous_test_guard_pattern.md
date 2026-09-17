---
name: vacuous-test-guard-pattern
description: This repo's grep-style spec tests (e.g. B-3/FR-007 unscoped-SELECT scan) guard against vacuous-pass by first asserting the scan found an expected source file before trusting an empty result; treat the absence of such a guard as a finding.
metadata:
  type: feedback
---

In `crates/aicortex-store/tests/schema.rs`
(`b3_fr007_no_statement_selects_a_scoped_table_without_its_scope`), the test
that greps crate source for unscoped `SELECT`s first asserts
`sources.iter().any(|(name, _)| name == "memory_repo.rs")` before trusting an
empty `problems` list. This is the right shape for any text-scanning
compliance test: an empty violations list is only meaningful once you've
shown the scanner actually looked at something. The same file also
positively tests the scanner itself against four synthetic violation shapes
and against comment/string edge cases
(`fr007_the_check_refuses_an_unscoped_select`), rather than only testing the
real crate source.

**Why this matters:** a text-scanning "gate" test (grep for a pattern, assert
no matches) is the single easiest kind of test to make vacuously pass, by a
`find`/path bug, an empty glob, or a scanner that never fires. This repo's
author already knows this and treats it as a required guard.

**How to apply:** when reviewing any new or changed regex/text-scan style
test in this codebase (ownership scans, unsafe scans, scope-predicate scans,
egress-declaration scans), check for both (a) a positive assertion that the
scan actually found the expected input file/pattern, and (b) a
synthetic-defect test proving the scanner fires on a known-bad shape. Flag
the absence of either as a vacuous-test risk, citing this pattern as the bar
already set elsewhere in the repo.
