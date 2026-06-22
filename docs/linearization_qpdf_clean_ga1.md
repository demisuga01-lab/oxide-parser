# Linearization Hint Tables - GA Prompt 1

GA Prompt 1 fixes the Prompt 11 v1.0 blocker: qpdf recognized Oxide output as
linearized, but reported hint-table warnings.

## Reproduced Warning

Before the fix, `qpdf --check target/ga-linearization/basicapi-linearized.pdf`
reported:

```text
WARNING: object count mismatch for page 0: hint table = 25; computed = 23
WARNING: page length mismatch for page 1: hint table = 303; computed length = 913 (offset = 4055)
WARNING: page 1: shared object 14: in hint table but not computed list
WARNING: page length mismatch for page 2: hint table = 651; computed length = 68403 (offset = 4503)
WARNING: page 2: shared object 15: in hint table but not computed list
qpdf: operation succeeded with warnings
```

`qpdf --show-linearization` showed the page-offset hint table counted 25
first-page objects, while qpdf's derived layout counted 23.

## Cause

The page dependency closure for a page stopped traversing non-target `/Page`
dictionaries, but inserted those page objects into the closure before stopping.
For `basicapi.pdf`, the first-page group therefore contained the page
dictionaries for later pages. That made:

- the page-offset hint table's first-page object count two objects too high;
- later page byte ranges start at page dictionaries that had already been
  placed inside the first-page group;
- shared-object identifiers refer to objects qpdf did not derive as shared for
  those later pages.

The hint stream bit-packing and `/H` offsets were structurally valid; the bad
input to the hint tables was the page grouping.

## Fix

`dependency_closure_for_linearization` now keeps a separate visited set and
skips non-target `/Page` dictionaries before inserting them into the closure.
Later page dictionaries stay in their own page groups, and the existing hint
table encoder then writes page object counts, page lengths, shared-object
references, `/H`, `/E`, `/T`, and `/L` from the final converged layout.

## qpdf-Clean Fixture Breadth

Each fixture below was linearized with `target/release/oxide.exe linearize`,
then validated with both `qpdf --check` and `qpdf --show-linearization`.
All checks exited 0 with zero warnings.

| Fixture | Coverage |
| --- | --- |
| `minimal.pdf` | single-page minimal document |
| `flate.pdf` | single-page compressed stream |
| `multi_stream.pdf` | multiple content streams |
| `basicapi.pdf` | multi-page document with shared fonts/resources |
| `tracemonkey.pdf` | larger realistic multi-page document |
| `form_160f.pdf` | form/AcroForm fixture |
| `target/ga-linearization/synthetic100.pdf` | generated 100-page document |

The regression is also covered by
`linearize_outputs_are_qpdf_clean_when_available`, which runs the same
qpdf-clean checks when qpdf is present on `PATH`.

Determinism was checked by linearizing `basicapi.pdf` twice and comparing
SHA-256 hashes; both outputs matched
`0B5670E022CE87943F6F415C6EF27CE88C966DECDFD2071E45E6173D7D7329F8`.
