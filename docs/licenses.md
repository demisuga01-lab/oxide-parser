# Third-Party Licenses & Attribution (NOTICE)

This is the consolidated attribution document for the Oxide PDF Toolkit. It
covers (1) the project's own license, (2) the bundled fonts, and (3) the Rust
crate dependency tree. Generated/verified with `cargo metadata` and the fonts'
embedded license metadata.

## 1. Oxide's own license

The Oxide PDF Toolkit (`oxide-engine`, `oxide-cli`, `oxide-server`) is licensed
under **`MIT OR Apache-2.0`** at the user's option. See [`LICENSE-MIT`](../LICENSE-MIT)
and [`LICENSE-APACHE`](../LICENSE-APACHE) at the repository root. Each crate
declares `license = "MIT OR Apache-2.0"` in its `Cargo.toml`.

## 2. Bundled fonts (embedded into `oxide-engine`)

These font programs are embedded as fallback/substitution fonts (see
[`crates/engine/fonts/README.md`](../crates/engine/fonts/README.md)).

| Font | License | License file |
|---|---|---|
| DejaVu Sans (`DejaVuSans.ttf`) | DejaVu / Bitstream Vera (permissive, MIT-like) + Arev; DejaVu changes public domain | [`crates/engine/fonts/LICENSE-DejaVu.txt`](../crates/engine/fonts/LICENSE-DejaVu.txt) |
| Liberation Sans / Serif / Mono (`Liberation*.ttf`, 12 files) | SIL Open Font License (OFL) 1.1 | [`crates/engine/fonts/LICENSE-Liberation.txt`](../crates/engine/fonts/LICENSE-Liberation.txt) |

Both are permissive (non-copyleft) and compatible with `MIT OR Apache-2.0`.
The full license texts ship in-repo as those licenses require.

## 3. Rust crate dependencies

A `cargo metadata` scan of the **entire resolved dependency tree** (260
third-party crates, excluding Oxide's own workspace crates) yields the following
license distribution — **all permissive, no forced copyleft**:

| Count | License (SPDX) |
|---:|---|
| 134 | MIT OR Apache-2.0 |
| 47 | MIT |
| 31 | Apache-2.0 OR MIT |
| 14 | MIT/Apache-2.0 |
| 6 | Unlicense OR MIT |
| 3 | Apache-2.0 |
| 3 | BSD-3-Clause |
| 3 | MIT OR Apache-2.0 OR Zlib |
| 3 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| 2 | Zlib OR Apache-2.0 OR MIT |
| 2 | BSD-2-Clause OR Apache-2.0 OR MIT |
| 1 each | 0BSD OR MIT OR Apache-2.0; BSD-2-Clause; CC0-1.0 OR MIT-0 OR Apache-2.0; (Apache-2.0 OR MIT) AND BSD-3-Clause; (MIT OR Apache-2.0) AND IJG; MIT AND BSD-3-Clause; MIT OR Zlib OR Apache-2.0; MIT OR Apache-2.0 OR LGPL-2.1-or-later; Apache-2.0 OR BSL-1.0; Zlib; (MIT OR Apache-2.0) AND Unicode-3.0 |

### Audit result — no copyleft conflict

- **No GPL/AGPL anywhere.** The only crate whose license string mentions any
  form of GPL is **`r-efi`** (`MIT OR Apache-2.0 OR LGPL-2.1-or-later`): a
  tri-licensed UEFI-target crate from which the MIT or Apache-2.0 option can be
  taken (so **no copyleft is forced**), and it is not compiled on the primary
  (Windows/x86-64) target.
- The `IJG` (libjpeg), `Unicode-3.0`, `BSD-*`, and `Zlib` components are all
  permissive and compatible with `MIT OR Apache-2.0`.
- Every crate declares a license (no `(none)` entries in the resolved tree).

**Conclusion:** the dependency tree is fully compatible with the project's
`MIT OR Apache-2.0` license. This is the differentiator vs Poppler's GPLv2 —
Oxide can be embedded in proprietary software without copyleft obligations.

## 4. C-toolchain note (pure-Rust status)

The library (`oxide-engine`) pulls **no C** at runtime. The `oxide-cli` and
`oxide-server` binaries pull three C-backed crates (`bzip2-sys`, `lzma-sys`,
`zstd-sys`) **only** via `zip`'s default features — a build-time C dependency
that is a one-line fix (`zip = { default-features = false, features =
["deflate"] }`). No `ring`/`openssl`/`cmake` anywhere. See the positioning doc
§D.4.

## Reproducing this audit

```
cargo metadata --format-version 1   # license field of every resolved package
# fonts: licenses are embedded in each .ttf 'name' table (IDs 0, 13, 14)
```
