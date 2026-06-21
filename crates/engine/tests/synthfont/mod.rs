//! Shared test helper: build a minimal, valid OpenType **variable** TrueType
//! font in-memory. Pure-Rust, deterministic, no external assets — used to prove
//! the variable-font outline/metrics interpolation path.
//!
//! The font has:
//!   - one `wght` axis, range 100..900, default 400;
//!   - two glyphs: gid0 `.notdef` (empty), gid1 a square (100,100)-(400,400)
//!     mapped from `'A'` via a format-4 cmap;
//!   - a `gvar` tuple that, at peak weight (normalized +1.0 == wght 900), expands
//!     the square outward from its centre by 150 units on every side;
//!   - an `HVAR` table that adds +300 to gid1's advance at peak weight.
//!
//! The serialization follows the OpenType spec directly (the crate only *reads*
//! these tables). Any unused-helper warnings are silenced because different test
//! files include different parts of this module.
#![allow(dead_code)]

const UPEM: u16 = 1000;

// ── big-endian byte writer ──────────────────────────────────────────────────
struct W {
    b: Vec<u8>,
}
impl W {
    fn new() -> Self {
        W { b: Vec::new() }
    }
    fn u16(&mut self, v: u16) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }
    fn u8(&mut self, v: u8) {
        self.b.push(v);
    }
    fn bytes(&mut self, s: &[u8]) {
        self.b.extend_from_slice(s);
    }
    /// 16.16 fixed.
    fn fixed(&mut self, v: f32) {
        self.i32((v * 65536.0).round() as i32);
    }
    fn pad4(&mut self) {
        while !self.b.len().is_multiple_of(4) {
            self.b.push(0);
        }
    }
}

fn u32b(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// glyf for gid0 (empty) + gid1 (square). Returns (glyf bytes, loca offsets).
fn build_glyf() -> (Vec<u8>, Vec<u32>) {
    let mut glyf = W::new();
    let mut loca = vec![0u32];

    // gid0 .notdef: empty.
    loca.push(glyf.b.len() as u32);

    // gid1: square, one contour, 4 on-curve points
    // p0(100,100) p1(400,100) p2(400,400) p3(100,400).
    glyf.i16(1); // numberOfContours
    glyf.i16(100); // xMin
    glyf.i16(100); // yMin
    glyf.i16(400); // xMax
    glyf.i16(400); // yMax
    glyf.u16(3); // endPtsOfContours[0]
    glyf.u16(0); // instructionLength
    for _ in 0..4 {
        glyf.u8(0x01); // ON_CURVE_POINT
    }
    // x deltas (int16, since X_SHORT/SAME flags unset): +100, +300, 0, -300.
    glyf.i16(100);
    glyf.i16(300);
    glyf.i16(0);
    glyf.i16(-300);
    // y deltas: +100, 0, +300, 0.
    glyf.i16(100);
    glyf.i16(0);
    glyf.i16(300);
    glyf.i16(0);
    glyf.pad4();
    loca.push(glyf.b.len() as u32);

    (glyf.b, loca)
}

/// Pack gvar deltas using the WORDS encoding (runs of up to 64).
fn pack_deltas(w: &mut W, deltas: &[i16]) {
    let mut i = 0;
    while i < deltas.len() {
        let run = (deltas.len() - i).min(64);
        w.u8(0x40 | (run as u8 - 1)); // DELTAS_ARE_WORDS | (run-1)
        for d in &deltas[i..i + run] {
            w.i16(*d);
        }
        i += run;
    }
}

/// gvar for one axis, expanding gid1's square outward at peak weight.
/// `n` = points including the 4 trailing phantom points.
fn build_gvar(n: u16) -> Vec<u8> {
    let mut hdr = W::new();
    hdr.u32(0x00010000); // version
    hdr.u16(1); // axisCount
    hdr.u16(0); // sharedTupleCount
    let shared_tuples_off_pos = hdr.b.len();
    hdr.u32(0); // sharedTuplesOffset (patched to a valid in-bounds offset)
    hdr.u16(2); // glyphCount
    hdr.u16(0); // flags: short offsets (value/2)
    let gv_array_off_pos = hdr.b.len();
    hdr.u32(0); // glyphVariationDataArrayOffset (patched)

    // Serialized data for gid1's single tuple: implicit "all points" (no shared/
    // private point numbers), so the data is JUST packed deltas: X then Y.
    let mut serialized = W::new();
    let x_deltas: Vec<i16> = {
        let mut v = vec![0i16; n as usize];
        v[0] = -150;
        v[1] = 150;
        v[2] = 150;
        v[3] = -150;
        v
    };
    let y_deltas: Vec<i16> = {
        let mut v = vec![0i16; n as usize];
        v[0] = -150;
        v[1] = -150;
        v[2] = 150;
        v[3] = 150;
        v
    };
    pack_deltas(&mut serialized, &x_deltas);
    pack_deltas(&mut serialized, &y_deltas);

    // GlyphVariationData for gid1.
    let mut glyph_data = W::new();
    glyph_data.u16(1); // tupleVariationCount = 1, no flags
    let data_off_pos = glyph_data.b.len();
    glyph_data.u16(0); // dataOffset (patched)
                       // TupleVariationHeader
    glyph_data.u16(serialized.b.len() as u16); // variationDataSize
    glyph_data.u16(0x8000); // tupleIndex flags: EMBEDDED_PEAK_TUPLE
    glyph_data.i16(0x4000); // peak = +1.0 (F2DOT14)
    let data_off = glyph_data.b.len() as u16;
    glyph_data.b[data_off_pos] = (data_off >> 8) as u8;
    glyph_data.b[data_off_pos + 1] = (data_off & 0xff) as u8;
    glyph_data.bytes(&serialized.b);
    if !glyph_data.b.len().is_multiple_of(2) {
        glyph_data.u8(0);
    }

    // glyph variation data array (gid0 empty, gid1 data) + short offsets /2.
    let gid1_len = glyph_data.b.len();
    let mut gv_array = W::new();
    gv_array.bytes(&glyph_data.b);

    let mut offsets = W::new();
    offsets.u16(0); // gid0 start
    offsets.u16(0); // gid1 start (gid0 empty)
    offsets.u16((gid1_len / 2) as u16); // end

    let mut out = W::new();
    out.bytes(&hdr.b);
    out.bytes(&offsets.b);
    out.pad4();
    let gv_array_off = out.b.len() as u32;
    out.bytes(&gv_array.b);

    out.b[gv_array_off_pos..gv_array_off_pos + 4].copy_from_slice(&u32b(gv_array_off));
    let so = u32b(out.b.len() as u32); // shared tuples offset: a valid in-bounds offset (count 0)
    out.b[shared_tuples_off_pos..shared_tuples_off_pos + 4].copy_from_slice(&so);
    out.b
}

fn build_fvar() -> Vec<u8> {
    let mut w = W::new();
    w.u16(1); // major
    w.u16(0); // minor
    w.u16(16); // axesArrayOffset
    w.u16(2); // reserved
    w.u16(1); // axisCount
    w.u16(20); // axisSize
    w.u16(0); // instanceCount
    w.u16(0); // instanceSize
    w.bytes(b"wght");
    w.fixed(100.0); // min
    w.fixed(400.0); // default
    w.fixed(900.0); // max
    w.u16(0); // flags
    w.u16(256); // nameID
    w.b
}

fn build_head() -> Vec<u8> {
    let mut w = W::new();
    w.fixed(1.0); // version
    w.fixed(1.0); // fontRevision
    w.u32(0); // checkSumAdjustment
    w.u32(0x5F0F3CF5); // magicNumber
    w.u16(0); // flags
    w.u16(UPEM);
    w.u32(0);
    w.u32(0); // created
    w.u32(0);
    w.u32(0); // modified
    w.i16(0);
    w.i16(0);
    w.i16(1000);
    w.i16(1000); // bbox
    w.u16(0); // macStyle
    w.u16(8); // lowestRecPPEM
    w.i16(2); // fontDirectionHint
    w.i16(0); // indexToLocFormat (short)
    w.i16(0); // glyphDataFormat
    w.b
}

fn build_hhea() -> Vec<u8> {
    let mut w = W::new();
    w.fixed(1.0);
    w.i16(800); // ascender
    w.i16(-200); // descender
    w.i16(0); // lineGap
    w.u16(900); // advanceWidthMax
    w.i16(0);
    w.i16(0);
    w.i16(1000);
    w.i16(1);
    w.i16(0);
    w.i16(0);
    w.i16(0);
    w.i16(0);
    w.i16(0);
    w.i16(0);
    w.i16(0); // metricDataFormat
    w.u16(2); // numberOfHMetrics
    w.b
}

fn build_hmtx() -> Vec<u8> {
    let mut w = W::new();
    w.u16(600); // gid0 advance
    w.i16(0); // gid0 lsb
    w.u16(600); // gid1 advance
    w.i16(100); // gid1 lsb
    w.b
}

fn build_maxp(num_glyphs: u16) -> Vec<u8> {
    let mut w = W::new();
    w.fixed(1.0); // version 1.0
    w.u16(num_glyphs);
    w.u16(10); // maxPoints
    w.u16(1); // maxContours
    w.u16(0);
    w.u16(0);
    w.u16(2); // maxZones
    w.u16(0);
    w.u16(0);
    w.u16(0);
    w.u16(0);
    w.u16(0);
    w.u16(0);
    w.u16(0);
    w.u16(0);
    w.b
}

fn build_loca(offsets: &[u32]) -> Vec<u8> {
    let mut w = W::new();
    for &o in offsets {
        w.u16((o / 2) as u16);
    }
    w.b
}

/// HVAR: gid1 advance grows +300 at peak weight (ItemVariationStore).
fn build_hvar() -> Vec<u8> {
    // VariationRegionList: 1 axis, 1 region (start 0, peak 1.0, end 1.0).
    let mut region_list = W::new();
    region_list.u16(1); // axisCount
    region_list.u16(1); // regionCount
    region_list.i16(0); // start
    region_list.i16(0x4000); // peak = 1.0
    region_list.i16(0x4000); // end = 1.0

    // ItemVariationData: 2 items, 1 region, deltas as words.
    let mut ivd = W::new();
    ivd.u16(2); // itemCount
    ivd.u16(1); // wordDeltaCount
    ivd.u16(1); // regionIndexCount
    ivd.u16(0); // regionIndexes[0]
    ivd.i16(0); // gid0 delta
    ivd.i16(300); // gid1 delta

    let mut ivs = W::new();
    ivs.u16(1); // format
    let region_off_pos = ivs.b.len();
    ivs.u32(0); // variationRegionListOffset
    ivs.u16(1); // itemVariationDataCount
    let ivd_off_pos = ivs.b.len();
    ivs.u32(0); // itemVariationDataOffsets[0]
    let region_off = ivs.b.len() as u32;
    ivs.bytes(&region_list.b);
    let ivd_off = ivs.b.len() as u32;
    ivs.bytes(&ivd.b);
    ivs.b[region_off_pos..region_off_pos + 4].copy_from_slice(&u32b(region_off));
    ivs.b[ivd_off_pos..ivd_off_pos + 4].copy_from_slice(&u32b(ivd_off));

    let mut hvar = W::new();
    hvar.u16(1); // major
    hvar.u16(0); // minor
    let ivs_off_pos = hvar.b.len();
    hvar.u32(0); // itemVariationStoreOffset
    hvar.u32(0); // advanceWidthMappingOffset = 0 => implicit gid->index
    hvar.u32(0); // lsbMappingOffset
    hvar.u32(0); // rsbMappingOffset
    let ivs_off = hvar.b.len() as u32;
    hvar.bytes(&ivs.b);
    hvar.b[ivs_off_pos..ivs_off_pos + 4].copy_from_slice(&u32b(ivs_off));
    hvar.b
}

fn build_cmap() -> Vec<u8> {
    // Format 4: map 'A' (0x41) -> gid 1.
    let mut sub = W::new();
    sub.u16(4); // format
    let len_pos = sub.b.len();
    sub.u16(0); // length (patched)
    sub.u16(0); // language
    sub.u16(2 * 2); // segCountX2 (2 segments)
    sub.u16(2); // searchRange
    sub.u16(0); // entrySelector
    sub.u16(0); // rangeShift
    sub.u16(0x41); // endCode[0]
    sub.u16(0xffff); // endCode[1]
    sub.u16(0); // reservedPad
    sub.u16(0x41); // startCode[0]
    sub.u16(0xffff); // startCode[1]
    sub.i16(1 - 0x41); // idDelta[0] => maps 0x41 to 1
    sub.i16(1); // idDelta[1]
    sub.u16(0); // idRangeOffset[0]
    sub.u16(0); // idRangeOffset[1]
    let len = sub.b.len() as u16;
    sub.b[len_pos] = (len >> 8) as u8;
    sub.b[len_pos + 1] = (len & 0xff) as u8;

    let mut w = W::new();
    w.u16(0); // version
    w.u16(1); // numTables
    w.u16(3); // platformID Windows
    w.u16(1); // encodingID Unicode BMP
    w.u32(12); // subtable offset
    w.bytes(&sub.b);
    w.b
}

fn build_name() -> Vec<u8> {
    let s: Vec<u16> = "Weight".encode_utf16().collect();
    let mut strdata = W::new();
    for u in &s {
        strdata.u16(*u);
    }
    let mut w = W::new();
    w.u16(0); // format
    w.u16(1); // count
    w.u16(6 + 12); // stringOffset
    w.u16(3); // platformID
    w.u16(1); // encodingID
    w.u16(0x0409); // languageID
    w.u16(256); // nameID
    w.u16((s.len() * 2) as u16); // length
    w.u16(0); // offset
    w.bytes(&strdata.b);
    w.b
}

fn assemble(mut tables: Vec<(&[u8; 4], Vec<u8>)>) -> Vec<u8> {
    let num = tables.len() as u16;
    tables.sort_by(|a, b| a.0.cmp(b.0));

    let mut search = 1u16;
    let mut entry_sel = 0u16;
    while search * 2 <= num {
        search *= 2;
        entry_sel += 1;
    }
    let search_range = search * 16;
    let range_shift = num * 16 - search_range;

    let mut header = W::new();
    header.u32(0x00010000); // sfnt version (TrueType)
    header.u16(num);
    header.u16(search_range);
    header.u16(entry_sel);
    header.u16(range_shift);

    let dir_size = 12 + 16 * tables.len();
    let mut offset = dir_size;
    let mut records = W::new();
    let mut body = W::new();
    for (tag, data) in &tables {
        records.bytes(*tag);
        records.u32(0); // checksum (ignored)
        records.u32(offset as u32);
        records.u32(data.len() as u32);
        body.bytes(data);
        let mut padded = data.len();
        while padded % 4 != 0 {
            body.u8(0);
            padded += 1;
        }
        offset += padded;
    }
    let mut out = W::new();
    out.bytes(&header.b);
    out.bytes(&records.b);
    out.bytes(&body.b);
    out.b
}

/// Build the complete synthetic `wght`-variable TrueType font.
pub fn build_weight_variable_font() -> Vec<u8> {
    let (glyf, loca) = build_glyf();
    let num_glyphs = 2u16;
    // gid1: 4 contour points + 4 phantom points.
    let gvar = build_gvar(4 + 4);
    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"head", build_head()),
        (b"hhea", build_hhea()),
        (b"maxp", build_maxp(num_glyphs)),
        (b"hmtx", build_hmtx()),
        (b"loca", build_loca(&loca)),
        (b"glyf", glyf),
        (b"cmap", build_cmap()),
        (b"name", build_name()),
        (b"fvar", build_fvar()),
        (b"gvar", gvar),
        (b"HVAR", build_hvar()),
    ];
    assemble(tables)
}
