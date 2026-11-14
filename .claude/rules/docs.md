---
paths:
  - "docs/**"
---

# Project documents

- `SPEC.md` is the user's original requirements, verbatim. Never edit it.
- `VERIFICATION_LOG.md`: append or supersede rows and never delete them. To change a finding, mark the old row
  `superseded YYYY-MM-DD → #N` and add a new row. Each row has a parameter, finding, applies-to (crate version, engine
  commit, server), source URL, date, confidence (`verified`, `inferred`, or `unverified`), and the constant or code location.
  Keep the "Discrepancies with SPEC" and "Remaining unverified" sections current.
- `DECISIONS.md`: ADRs are numbered and never renumbered. Status is Proposed, Accepted, or Superseded. Each records context,
  options, decision, consequences, and the evidence (measurements, sources). The user accepts ADRs. You propose them.
- `STATUS.md`: keep it short. Show the current phase, a checklist, and a newest-first log with one line per module or gate, including
  evidence (commands run and key numbers), blockers, and open decisions.
- `TUNING.md` mirrors `config.rs` field for field, with default, unit, rationale and safe range. The doc-sync test guards it.
- Update a document only when repository evidence supports the change.
