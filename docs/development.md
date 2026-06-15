# Development notes

Operational/process notes for working on the Oxide PDF Toolkit. Nothing here is
part of the shipped product (the `oxide-engine`/`oxide-cli`/`oxide-server`
crates); it documents local-only files and dev scaffolding.

## `CLAUDE-CODE-CLI.bat` — local-only, never commit

`CLAUDE-CODE-CLI.bat` at the repo root is a **local-only developer convenience**
launcher. It routes Claude Code's auth through a third-party proxy server and
supplies/prompts for an API key at runtime.

- It is **intentionally `.gitignored`** and is **not tracked** (verified:
  `git check-ignore` matches, `git ls-files` returns nothing for it).
- **Never commit it.** It contains (or references) a secret. The `.gitignore`
  entry exists precisely so an edited copy can never leak a key into history.
- **Treat any key in it as a secret.** If it is ever exposed (committed,
  pasted, shared), **rotate the key immediately** at the proxy/provider.
- It is left untouched on disk; this note only records its disposition.

## `skills/` and `references/` — development scaffolding

The `skills/` and `references/` directories are **development/process
materials**, not product source:

- `skills/` — agent "skills" (e.g. `test-driven-development`,
  `security-and-hardening`, `code-review-and-quality`): prompt/process guidance
  used while developing the toolkit.
- `references/` — engineering checklists (`security-checklist.md`,
  `performance-checklist.md`, `testing-patterns.md`, etc.).

They are **not** referenced by, compiled into, or required by any crate, and
they do not ship with the toolkit.

**Recommendation:** since they are dev-only scaffolding, the cleanest long-term
disposition is to `.gitignore` them (or move them under a clearly-labelled
`dev/` area) so a public checkout contains only product + product docs. They are
**deliberately left in place and untouched** here — removing or relocating a
contributor's working materials is the repository owner's call; this note
records the decision point rather than making it unilaterally. If you want them
out of version control, add `skills/` and `references/` to `.gitignore` (no code
change is required — nothing depends on them).

## Scratch files

`ROUND6-SCRATCH.md` and `_ox.txt` at the repo root are leftover scratch notes
from development. They are not part of the product. They remain untracked; add
them to `.gitignore` or delete them at your discretion (nothing depends on them).
