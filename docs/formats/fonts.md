# Fonts: TrueType, OpenType (CFF), WOFF, collections

Crates: `oc-font` (parsing, outlines, conversions), `oc-raster` (drawing the specimen), `oc-engine/src/font.rs` (routes and options).

## What is read
- sfnt containers: TrueType (`glyf`), OpenType with CFF (`CFF `), `true`/`typ1` tags, and TrueType collections (one member chosen with `font_index`).
- `head`, `hhea`, `hmtx`, `maxp`, `name`, `OS/2`, `post` (formats 1 and 2 names), `kern` (format 0).
- `cmap` subtables of formats 0, 4, 6, 12 and 13 (binary search for lookup, full enumeration for listings).
- `glyf` simple and composite glyphs (offsets and 2x2 transforms; component point matching is not done).
- CFF: INDEX and DICT parsing, charsets (formats 0 to 2 and predefined), CID-keyed fonts (FDArray, FDSelect 0 and 3), built-in encodings, local and global subroutines, the full Type 2 operator set including flex, arithmetic, hint masks and `seac`-style `endchar`.
- WOFF 1.0: tables are inflated and the sfnt is rebuilt with correct checksums.

## What is written
| Route | How |
| --- | --- |
| TTF, OTF to WOFF | each table deflated when that is smaller; the directory carries the original checksums |
| WOFF to TTF or OTF | tables inflated, `head.checkSumAdjustment` recomputed |
| OTF to TTF | each cubic segment becomes quadratic segments within `curve_tolerance` (default 0.5/1000 em); `glyf`, `loca`, a version 1.0 `maxp` and a `post` 3.0 are written |
| TTF to OTF | every quadratic becomes an exact cubic; Type 2 charstrings and a CFF table are written; the glyph names of `post` become the CFF charset |
| any font to an image | a specimen sheet: name, alphabet, digits and a pangram at five sizes, with `kern` pairs applied |

Oracles (test time only): fontTools reads every output and its outline areas and advances match the source; Chromium's font sanitiser loads each one (`tools/font_interop.py`, 46 checks).

## Not done
- WOFF2 (needs a Brotli decoder and its static dictionary, which is not written), Type 1 (PFA/PFB) and bitmap-only fonts as inputs or outputs.
- Variable fonts: only the default instance is converted, with a warning; `gvar`/`fvar` are dropped when the outline type changes.
- Hinting: TrueType instructions are dropped when making CFF, and CFF hints are not carried to TrueType (a note says so).
- Layout features: the specimen applies `kern` pairs only; GSUB/GPOS shaping is a Phase 6b item.
- Glyph names above SID 228 in CFF's built-in string table are unnamed (custom strings work).
