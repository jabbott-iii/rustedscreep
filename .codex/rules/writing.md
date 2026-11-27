---
paths:
  - "**/*.md"
  - "docs/**"
  - "**/*.rs"
---

# Rules for writing documentation

These rules cover everything written for people to read:

- the documents in `docs/`
- `README.md`, `KIT_README.md`, and the fixtures' READMEs
- `AGENTS.md`, and the skills in `.agents/skills/` and the rules and agents in `.codex/`
- Rust doc comments (`///` and `//!`), which `cargo doc` turns into the API reference

Two things are not covered:

- `docs/SPEC.md` is the user's text and is never edited (`.codex/rules/docs.md`).
- The bot's console and log text in code is program output, not documentation. The sandbox sims match it word for
  word, so change it only together with its sim.

AGENTS.md's definition of done and `.codex/rules/docs.md` say which documents a change must update and what each one
holds. This file says how to write them.

Write for two readers at once: a second-year computer science student who is new to Screeps, and a senior engineer
who needs every detail.

## Keep jargon down

- **Use a plain phrase** whenever it is just as exact.
- **Use a technical term only** when it carries meaning a plain phrase can't, or when the reader needs the term to
  search for it or to use the tool.
- **Explain each term a student might not know** the first time it appears, in a short clause or parenthetical. Then
  use it freely. For example: "pre-spawn replacement (ordering a creep's successor early enough that it is in place
  when the old creep dies of age)".
- **Spell out acronyms on first use,** as in "room controller level (RCL)". Universal ones such as CPU, API, and JSON
  don't need it.
- **Drop insider shorthand:**
  - slang such as "footgun" and "yak shave"
  - a bare reference that doesn't say what the record decided, what the task builds, or what the row found. This
    covers architecture decision records (ADR-028), roadmap tasks (P5.2), and verification-log rows (V174). Write
    "ADR-028 (the wasm's base64 is split across several modules, so none is larger than 905709 bytes, the largest
    module the GitHub link has delivered intact)", "P5.2 (pre-spawn replacement)", and "V174 (the spawn places the
    new creep once the game time reaches `spawnTime - 1`)".
  - In `docs/STATUS.md`, which `.codex/rules/docs.md` asks to keep short, a few words in the parentheses are
    enough: "ADR-028 (the split wasm)".

## Don't give up meaning

- **Accuracy wins over simplicity.** Keep the exact numbers, limits, terms, and caveats. The source-of-truth rule in
  AGENTS.md still applies to every game number you write.
- **If a simpler sentence would be wrong or misleading,** keep the precise wording and add an explanation beside it.
- **Don't remove details senior engineers need.** Explain them so everyone else can follow.

## Help newer readers follow

- **Explain why** a design choice was made, not only what it is.
- **Introduce one idea at a time,** with concrete examples. An analogy goes next to the real explanation, never in
  place of it.
- **Leave out "obviously" and "trivially".**
- **Keep paragraphs short,** and write headings that make the page easy to skim: "Why miners are ordered before they
  die", not "Notes".

## In doc comments

A reader lands on one item's page, not at the top of the crate. Explain a term once, in the module docs (`//!`) or on
the item that defines it. Elsewhere, link to that explanation with an intra-doc link such as ``[`crate::spawning::Body`]`` or
``[`crate::spawning::lead_ticks`]`` instead of repeating it on every item. A link resolves from the item's own scope, so a bare
``[`Body`]`` works only where `Body` is in scope, and from another crate the path starts with the crate's name.

## Use American English

- **Spelling:**
  - -or, not -our: color, behavior, neighbor, honor
  - -er, not -re: center, centered
  - -ize, not -ise, where British English uses -ise: initialize, serialize, normalize
  - one l before -ed and -ing when the word's last syllable isn't stressed: canceled, labeled, modeling, tunneling
    (but controlled and compelled keep two)
  - and gray, catalog, spelled (not spelt), acknowledgment, and license as both noun and verb
- **Grammar:** write complete sentences whose subjects and verbs agree. Keep one tense for the same kind of statement.
- **Punctuation:** use the serial comma (the comma before the final "and" or "or" in a list of three or more), as in
  "numbers, limits, and caveats". Most existing pages leave it out. Add it in the sentences you write or change.
- **Quote names exactly:** code identifiers, file names, the fields of APIs and file formats, command and console
  output, and third-party names keep their own spelling, even when it is British. For example, the function
  `keeps_neighbours_connected` keeps its name, and renaming it would be an unrelated change.
- **Fix only the text you change.** Don't sweep a whole page for spelling or commas as part of another task
  (AGENTS.md's working agreement: the smallest complete change, with no formatting sweeps). In `VERIFICATION_LOG.md`
  this means the rows you add, since old rows are only superseded. Mention the ones you notice in your summary
  instead.

## Before finishing: the two-reader check

1. Reread the draft as a second-year computer science student. Look for undefined terms and skipped steps.
2. Reread it as a senior engineer. Look for anything vague, oversimplified, or wrong.
