# Differential Regression Seeds

This directory stores minimized PDFs for confirmed Oxide wrong-output bugs
found by the differential harness.

Policy:

- Add only minimized, reviewed PDFs that reproduce a confirmed Oxide bug.
- Keep the seed filename descriptive, for example
  `page-count-mismatch-nested-pages.pdf`.
- Run `python scripts/differential_fuzz.py --cases 0` before committing; the
  harness always replays this directory before generated cases.
- Legitimate reference/Oxide differences belong in documentation or harness
  suppression logic, not in this regression directory.

There are no confirmed wrong-output regression seeds yet.
