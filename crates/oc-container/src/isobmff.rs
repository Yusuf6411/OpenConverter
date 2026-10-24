//! ISO base media file format (MP4, M4A, MOV, 3GP): box walking, sample tables and a writer.
//!
//! The parser flattens each `trak` into a list of samples (file offset, size, duration), which
//! is all a codec needs. Fragmented files (`moof`/`traf`/`trun`) are folded into the same list.
use oc_core::{Error, Limits, Result};

pub type FourCc = [u8; 4];

pub const fn cc(s: &[u8; 4]) -> FourCc {
    *s
}

/// One box: its type and its body (without the header).
#[derive(Debug, Clone, Copy)]
pub struct BoxRef<'a> {
    pub kind: FourCc,
    pub body: &'a [u8],
    /// Offset of the box header in the buffer that was walked.
    pub start: usize,
}

pub fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
pub fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(o)?,
        *b.get(o + 1)?,
        *b.get(o + 2)?,
        *b.get(o + 3)?,
    ]))
}
pub fn be64(b: &[u8], o: usize) -> Option<u64> {
    Some((u64::from(be32(b, o)?) << 32) | u64::from(be32(b, o + 4)?))
}

/// Walk the boxes of a buffer. A truncated last box is clipped to the buffer; garbage ends the
/// walk rather than failing, because real files often carry junk after `mdat`.
pub fn boxes(data: &[u8]) -> Vec<BoxRef<'_>> {
    let mut v = Vec::new();
    let mut o = 0usize;
    while o + 8 <= data.len() {
        let Some(size32) = be32(data, o) else { break };
        let mut kind = [0u8; 4];
        kind.copy_from_slice(&data[o + 4..o + 8]);
        let (hdr, size) = match size32 {
            0 => (8usize, (data.len() - o) as u64),
            1 => match be64(data, o + 8) {
                Some(s) => (16usize, s),
                None => break,
            },
            n => (8usize, u64::from(n)),
        };
        if size < hdr as u64 {
            break;
        }
        let end = (o as u64).saturating_add(size).min(data.len() as u64) as usize;
        v.push(BoxRef {
            kind,
            body: &data[o + hdr..end],
            start: o,
        });
        o = end;
    }
    v
}

/// First child of `kind`.
pub fn child<'a>(data: &'a [u8], kind: &FourCc) -> Option<BoxRef<'a>> {
    boxes(data).into_iter().find(|b| &b.kind == kind)
}

/// Follow a path of box types.
pub fn path<'a>(data: &'a [u8], p: &[&FourCc]) -> Option<BoxRef<'a>> {
    let mut cur = data;
    let mut last = None;
    for k in p {
        let b = child(cur, k)?;
        cur = b.body;
        last = Some(b);
    }
    last
}

/// One sample of a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub duration: u32,
    /// Offset of the composition time from the decode time (video with B-frames).
    pub cts_offset: i32,
    pub sync: bool,
}

#[derive(Debug, Clone)]
pub struct Track<'a> {
    pub id: u32,
    /// `soun`, `vide`, `subt`, `text`, ...
    pub handler: FourCc,
    pub timescale: u32,
    pub duration: u64,
    /// ISO 639-2/T language, three lower-case letters ("und" when unset).
    pub language: [u8; 3],
    /// Sample entry type (`alac`, `mp4a`, `avc1`, ...).
    pub codec: FourCc,
    /// The whole sample entry body after its box header.
    pub entry: &'a [u8],
    pub samples: Vec<Sample>,
    /// The `edts/elst` entries: (segment duration in movie time, media time, rate).
    pub edits: Vec<(u64, i64)>,
    /// Display rotation in degrees clockwise from the `tkhd` matrix (0, 90, 180 or 270).
    pub rotation: u16,
}

#[derive(Debug, Clone)]
pub struct Movie<'a> {
    pub brand: FourCc,
    pub compatible: Vec<FourCc>,
    pub timescale: u32,
    pub duration: u64,
    pub tracks: Vec<Track<'a>>,
    /// Body of the `ilst` box under `moov/udta/meta`.
    pub ilst: Option<&'a [u8]>,
    /// Whether the file used movie fragments.
    pub fragmented: bool,
}

fn full(body: &[u8]) -> Option<(u8, &[u8])> {
    Some((*body.first()?, body.get(4..)?))
}

/// Sniff for an ISO media file: `ftyp` first, or a `moov`/`mdat`/`free` start.
pub fn sniff(data: &[u8]) -> bool {
    data.len() >= 12
        && matches!(
            &data[4..8],
            b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip"
        )
}

pub fn parse<'a>(data: &'a [u8], limits: &Limits) -> Result<Movie<'a>> {
    let top = boxes(data);
    let mut movie = Movie {
        brand: *b"    ",
        compatible: Vec::new(),
        timescale: 0,
        duration: 0,
        tracks: Vec::new(),
        ilst: None,
        fragmented: false,
    };
    if let Some(f) = top.iter().find(|b| &b.kind == b"ftyp") {
        if let Some(b) = f.body.get(..4) {
            movie.brand.copy_from_slice(b);
        }
        movie.compatible = f
            .body
            .get(8..)
            .unwrap_or(&[])
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
    }
    let moov = top
        .iter()
        .find(|b| &b.kind == b"moov")
        .ok_or_else(|| Error::corrupt("no moov box"))?;
    if let Some((ver, mvhd)) = child(moov.body, b"mvhd").and_then(|b| full(b.body)) {
        if ver == 1 {
            movie.timescale = be32(mvhd, 16).unwrap_or(0);
            movie.duration = be64(mvhd, 20).unwrap_or(0);
        } else {
            movie.timescale = be32(mvhd, 8).unwrap_or(0);
            movie.duration = u64::from(be32(mvhd, 12).unwrap_or(0));
        }
    }
    for b in boxes(moov.body) {
        match &b.kind {
            b"trak" => {
                if let Some(t) = parse_track(b.body, data, limits)? {
                    movie.tracks.push(t);
                }
            }
            b"udta" => {
                if let Some(meta) = child(b.body, b"meta") {
                    // `meta` is a full box in ISO files and a plain box in old QuickTime
                    let inner = meta.body.get(4..).unwrap_or(&[]);
                    let found = child(inner, b"ilst").or_else(|| child(meta.body, b"ilst"));
                    movie.ilst = found.map(|i| i.body);
                }
            }
            b"meta" if movie.ilst.is_none() => {
                let inner = b.body.get(4..).unwrap_or(&[]);
                movie.ilst = child(inner, b"ilst").map(|i| i.body);
            }
            _ => {}
        }
    }
    if child(moov.body, b"mvex").is_some() {
        movie.fragmented = true;
        parse_fragments(&top, data, &mut movie, limits)?;
    }
    Ok(movie)
}

fn parse_track<'a>(trak: &'a [u8], file: &'a [u8], limits: &Limits) -> Result<Option<Track<'a>>> {
    let Some(tkhd) = child(trak, b"tkhd") else {
        return Ok(None);
    };
    let id = match full(tkhd.body) {
        Some((1, b)) => be32(b, 16),
        Some((_, b)) => be32(b, 8),
        None => None,
    }
    .unwrap_or(0);
    let rotation = full(tkhd.body)
        .and_then(|(ver, b)| {
            let m = if ver == 1 { 48 } else { 36 };
            let a = be32(b, m)? as i32;
            let bb = be32(b, m + 4)? as i32;
            Some(match (a, bb) {
                (0, v) if v > 0 => 90,
                (v, 0) if v < 0 => 180,
                (0, v) if v < 0 => 270,
                _ => 0,
            })
        })
        .unwrap_or(0);
    let Some(mdia) = child(trak, b"mdia") else {
        return Ok(None);
    };
    let Some((ver, mdhd)) = child(mdia.body, b"mdhd").and_then(|b| full(b.body)) else {
        return Ok(None);
    };
    let (timescale, duration, lang_off) = if ver == 1 {
        (be32(mdhd, 16), be64(mdhd, 20), 28)
    } else {
        (be32(mdhd, 8), be32(mdhd, 12).map(u64::from), 16)
    };
    let lang = be16(mdhd, lang_off).unwrap_or(0x55C4);
    let language = [
        ((lang >> 10) & 31) as u8 + 0x60,
        ((lang >> 5) & 31) as u8 + 0x60,
        (lang & 31) as u8 + 0x60,
    ];
    let mut handler = *b"    ";
    if let Some((_, h)) = child(mdia.body, b"hdlr").and_then(|b| full(b.body)) {
        if let Some(t) = h.get(4..8) {
            handler.copy_from_slice(t);
        }
    }
    let Some(stbl) = path(mdia.body, &[b"minf", b"stbl"]) else {
        return Ok(None);
    };
    let Some(stsd) = child(stbl.body, b"stsd") else {
        return Ok(None);
    };
    let entries = stsd.body.get(8..).unwrap_or(&[]);
    let Some(entry) = boxes(entries).into_iter().next() else {
        return Ok(None);
    };
    let samples = sample_table(stbl.body, limits)?;
    let mut edits = Vec::new();
    if let Some((ver, elst)) = path(trak, &[b"edts", b"elst"]).and_then(|b| full(b.body)) {
        let n = be32(elst, 0).unwrap_or(0) as usize;
        let step = if ver == 1 { 20 } else { 12 };
        for i in 0..n.min(elst.len() / step.max(1)) {
            let o = 4 + i * step;
            let e = if ver == 1 {
                be64(elst, o).zip(be64(elst, o + 8).map(|v| v as i64))
            } else {
                be32(elst, o)
                    .zip(be32(elst, o + 4).map(|v| i64::from(v as i32)))
                    .map(|(a, b)| (u64::from(a), b))
            };
            if let Some(e) = e {
                edits.push(e);
            }
        }
    }
    let _ = file;
    Ok(Some(Track {
        id,
        handler,
        timescale: timescale.unwrap_or(0),
        duration: duration.unwrap_or(0),
        language,
        codec: entry.kind,
        entry: entry.body,
        samples,
        edits,
        rotation,
    }))
}

fn sample_table(stbl: &[u8], limits: &Limits) -> Result<Vec<Sample>> {
    let table = |k: &FourCc| child(stbl, k).and_then(|b| full(b.body));
    // sizes
    let (fixed, count, sizes): (u32, usize, &[u8]) = match table(b"stsz") {
        Some((_, b)) => (
            be32(b, 0).unwrap_or(0),
            be32(b, 4).unwrap_or(0) as usize,
            b.get(8..).unwrap_or(&[]),
        ),
        None => match table(b"stz2") {
            // compact sizes: field_size in the low byte of the word before the count
            Some((_, b)) => (0, be32(b, 4).unwrap_or(0) as usize, b),
            None => (0, 0, &[]),
        },
    };
    limits.check_entries(count as u64)?;
    limits.check_alloc(count as u64 * 32)?;
    if fixed == 0 && sizes.len() / 4 < count && child(stbl, b"stz2").is_none() {
        return Err(Error::corrupt("stsz is shorter than its sample count"));
    }
    let size_of = |i: usize| -> u32 {
        if fixed != 0 {
            fixed
        } else {
            be32(sizes, i * 4).unwrap_or(0)
        }
    };
    // chunk offsets
    let offsets: Vec<u64> = if let Some((_, b)) = table(b"stco") {
        let n = be32(b, 0).unwrap_or(0) as usize;
        (0..n.min(b.len() / 4))
            .filter_map(|i| be32(b, 4 + i * 4).map(u64::from))
            .collect()
    } else if let Some((_, b)) = table(b"co64") {
        let n = be32(b, 0).unwrap_or(0) as usize;
        (0..n.min(b.len() / 8))
            .filter_map(|i| be64(b, 4 + i * 8))
            .collect()
    } else {
        Vec::new()
    };
    // sample-to-chunk
    let mut stsc: Vec<(u32, u32)> = Vec::new();
    if let Some((_, b)) = table(b"stsc") {
        let n = be32(b, 0).unwrap_or(0) as usize;
        for i in 0..n.min(b.len() / 12) {
            if let (Some(first), Some(per)) = (be32(b, 4 + i * 12), be32(b, 8 + i * 12)) {
                stsc.push((first, per));
            }
        }
    }
    // decode durations
    let mut durations: Vec<u32> = Vec::with_capacity(count.min(1 << 24));
    if let Some((_, b)) = table(b"stts") {
        let n = be32(b, 0).unwrap_or(0) as usize;
        for i in 0..n.min(b.len() / 8) {
            if let (Some(c), Some(d)) = (be32(b, 4 + i * 8), be32(b, 8 + i * 8)) {
                if durations.len() as u64 + u64::from(c) > count as u64 {
                    break;
                }
                durations.extend(std::iter::repeat_n(d, c as usize));
            }
        }
    }
    let mut cts: Vec<i32> = Vec::new();
    if let Some((ver, b)) = table(b"ctts") {
        let n = be32(b, 0).unwrap_or(0) as usize;
        for i in 0..n.min(b.len() / 8) {
            if let (Some(c), Some(d)) = (be32(b, 4 + i * 8), be32(b, 8 + i * 8)) {
                if cts.len() as u64 + u64::from(c) > count as u64 {
                    break;
                }
                let d = if ver == 0 && d > i32::MAX as u32 {
                    0
                } else {
                    d as i32
                };
                cts.extend(std::iter::repeat_n(d, c as usize));
            }
        }
    }
    // sync samples: all are sync when the table is absent
    let sync: Option<Vec<u32>> = table(b"stss").map(|(_, b)| {
        let n = be32(b, 0).unwrap_or(0) as usize;
        (0..n.min(b.len() / 4))
            .filter_map(|i| be32(b, 4 + i * 4))
            .collect()
    });
    let mut samples = Vec::with_capacity(count.min(1 << 24));
    let mut idx = 0usize;
    'chunks: for (ci, &off) in offsets.iter().enumerate() {
        let chunk_no = ci as u32 + 1;
        let per = stsc
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk_no)
            .map_or(1, |(_, p)| *p);
        let mut o = off;
        for _ in 0..per {
            if idx >= count {
                break 'chunks;
            }
            let size = size_of(idx);
            samples.push(Sample {
                offset: o,
                size,
                duration: durations.get(idx).copied().unwrap_or(0),
                cts_offset: cts.get(idx).copied().unwrap_or(0),
                sync: sync
                    .as_ref()
                    .is_none_or(|s| s.binary_search(&(idx as u32 + 1)).is_ok()),
            });
            o += u64::from(size);
            idx += 1;
        }
    }
    Ok(samples)
}

/// Fold movie fragments into the tracks' sample lists.
fn parse_fragments<'a>(
    top: &[BoxRef<'a>],
    file: &'a [u8],
    movie: &mut Movie<'a>,
    limits: &Limits,
) -> Result<()> {
    // trex defaults
    let mut defaults: Vec<(u32, u32, u32)> = Vec::new(); // track, duration, size
    if let Some(moov) = top.iter().find(|b| &b.kind == b"moov") {
        if let Some(mvex) = child(moov.body, b"mvex") {
            for t in boxes(mvex.body).iter().filter(|b| &b.kind == b"trex") {
                let b = t.body;
                if let (Some(id), Some(d), Some(s)) = (be32(b, 4), be32(b, 12), be32(b, 16)) {
                    defaults.push((id, d, s));
                }
            }
        }
    }
    for moof in top.iter().filter(|b| &b.kind == b"moof") {
        for traf in boxes(moof.body).iter().filter(|b| &b.kind == b"traf") {
            let Some((tf_flags, tfhd)) = child(traf.body, b"tfhd")
                .map(|b| (be32(b.body, 0).unwrap_or(0) & 0x00FF_FFFF, b.body))
            else {
                continue;
            };
            let track_id = be32(tfhd, 4).unwrap_or(0);
            let mut o = 8usize;
            let mut base = moof.start as u64;
            if tf_flags & 1 != 0 {
                base = be64(tfhd, o).unwrap_or(base);
                o += 8;
            }
            if tf_flags & 2 != 0 {
                o += 4;
            }
            let (def_d, def_s) = defaults
                .iter()
                .find(|d| d.0 == track_id)
                .map_or((0, 0), |d| (d.1, d.2));
            let mut dur = def_d;
            let mut siz = def_s;
            if tf_flags & 8 != 0 {
                dur = be32(tfhd, o).unwrap_or(dur);
                o += 4;
            }
            if tf_flags & 0x10 != 0 {
                siz = be32(tfhd, o).unwrap_or(siz);
            }
            let Some(track) = movie.tracks.iter_mut().find(|t| t.id == track_id) else {
                continue;
            };
            let mut next = base;
            for trun in boxes(traf.body).iter().filter(|b| &b.kind == b"trun") {
                let b = trun.body;
                let flags = be32(b, 0).unwrap_or(0) & 0x00FF_FFFF;
                let n = be32(b, 4).unwrap_or(0) as usize;
                limits.check_entries((track.samples.len() + n) as u64)?;
                let mut p = 8usize;
                let mut data_off = next;
                if flags & 1 != 0 {
                    data_off = (base as i64 + i64::from(be32(b, p).unwrap_or(0) as i32)) as u64;
                    p += 4;
                }
                let mut first_flags = None;
                if flags & 4 != 0 {
                    first_flags = be32(b, p);
                    p += 4;
                }
                let mut off = data_off;
                for i in 0..n {
                    let mut d = dur;
                    let mut s = siz;
                    let mut sf = if i == 0 { first_flags } else { None };
                    let mut c = 0i32;
                    if flags & 0x100 != 0 {
                        d = be32(b, p).ok_or_else(|| Error::truncated("trun"))?;
                        p += 4;
                    }
                    if flags & 0x200 != 0 {
                        s = be32(b, p).ok_or_else(|| Error::truncated("trun"))?;
                        p += 4;
                    }
                    if flags & 0x400 != 0 {
                        let v = be32(b, p).ok_or_else(|| Error::truncated("trun"))?;
                        if i != 0 || first_flags.is_none() {
                            sf = Some(v);
                        }
                        p += 4;
                    }
                    if flags & 0x800 != 0 {
                        c = be32(b, p).ok_or_else(|| Error::truncated("trun"))? as i32;
                        p += 4;
                    }
                    track.samples.push(Sample {
                        offset: off,
                        size: s,
                        duration: d,
                        cts_offset: c,
                        // bit 16 = "is not a sync sample"
                        sync: sf.is_none_or(|f| f & 0x0001_0000 == 0),
                    });
                    off += u64::from(s);
                }
                next = off;
            }
        }
    }
    let _ = file;
    Ok(())
}

/// The bytes of one sample, bounds-checked against the file.
pub fn sample_bytes<'a>(file: &'a [u8], s: &Sample) -> Result<&'a [u8]> {
    let start = usize::try_from(s.offset).map_err(|_| Error::corrupt("sample offset"))?;
    let end = start
        .checked_add(s.size as usize)
        .ok_or_else(|| Error::corrupt("sample size"))?;
    file.get(start..end)
        .ok_or_else(|| Error::truncated("sample lies past the end of the file"))
}

// ---------------------------------------------------------------------------------- writing

/// Builds nested boxes.
#[derive(Debug, Default, Clone)]
pub struct BoxWriter {
    pub buf: Vec<u8>,
}

impl BoxWriter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }
    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }
    /// Write a box whose body `f` fills in.
    pub fn boxed(&mut self, kind: &FourCc, f: impl FnOnce(&mut BoxWriter)) -> &mut Self {
        let mut inner = BoxWriter::new();
        f(&mut inner);
        let total = inner.buf.len() as u64 + 8;
        if total > u64::from(u32::MAX) {
            self.u32(1).bytes(kind).u64(total + 8);
        } else {
            self.u32(total as u32).bytes(kind);
        }
        self.buf.extend_from_slice(&inner.buf);
        self
    }
    /// A full box: version and 24 bits of flags first.
    pub fn full(
        &mut self,
        kind: &FourCc,
        version: u8,
        flags: u32,
        f: impl FnOnce(&mut BoxWriter),
    ) -> &mut Self {
        self.boxed(kind, |w| {
            w.u32((u32::from(version) << 24) | (flags & 0x00FF_FFFF));
            f(w);
        })
    }
}

/// Pack a language code ("eng") into the 15-bit form of `mdhd`.
pub fn pack_language(lang: [u8; 3]) -> u16 {
    let c = |b: u8| u16::from(b.wrapping_sub(0x60) & 31);
    (c(lang[0]) << 10) | (c(lang[1]) << 5) | c(lang[2])
}

/// What the audio writer needs to describe one audio track.
pub struct AudioTrackSpec<'a> {
    pub timescale: u32,
    pub codec: FourCc,
    pub channels: u16,
    pub sample_bits: u16,
    pub sample_rate: u32,
    /// Boxes that follow the fixed audio sample entry fields (`alac`, `esds`, `dOps`, ...).
    pub entry_extra: &'a [u8],
    /// One `(size, duration)` per sample, in timescale units.
    pub samples: &'a [(u32, u32)],
    /// Ready-made `ilst` body, if any.
    pub ilst: Option<&'a [u8]>,
    pub brand: FourCc,
}

/// Write a complete audio-only MP4 file: `ftyp`, `moov`, `mdat`, with the index in front so the
/// file streams. `payload` is every sample's bytes back to back.
pub fn write_audio(spec: &AudioTrackSpec<'_>, payload: &[u8]) -> Vec<u8> {
    let ftyp = |w: &mut BoxWriter| {
        w.boxed(b"ftyp", |b| {
            b.bytes(&spec.brand)
                .u32(0)
                .bytes(&spec.brand)
                .bytes(b"mp42")
                .bytes(b"isom");
        });
    };
    let moov = |chunk_offset: u32| -> Vec<u8> {
        let total: u64 = spec.samples.iter().map(|s| u64::from(s.1)).sum();
        let mut w = BoxWriter::new();
        w.boxed(b"moov", |m| {
            m.full(b"mvhd", 0, 0, |b| {
                b.u32(0)
                    .u32(0)
                    .u32(spec.timescale)
                    .u32(total.min(u64::from(u32::MAX)) as u32);
                b.u32(0x0001_0000).u16(0x0100).zeros(10);
                for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
                    b.u32(v);
                }
                b.zeros(24).u32(2);
            });
            m.boxed(b"trak", |t| {
                t.full(b"tkhd", 0, 3, |b| {
                    b.u32(0).u32(0).u32(1).u32(0);
                    b.u32(total.min(u64::from(u32::MAX)) as u32);
                    b.zeros(8).u16(0).u16(0).u16(0x0100).u16(0);
                    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
                        b.u32(v);
                    }
                    b.u32(0).u32(0);
                });
                t.boxed(b"mdia", |d| {
                    d.full(b"mdhd", 0, 0, |b| {
                        b.u32(0).u32(0).u32(spec.timescale);
                        b.u32(total.min(u64::from(u32::MAX)) as u32);
                        b.u16(pack_language(*b"und")).u16(0);
                    });
                    d.full(b"hdlr", 0, 0, |b| {
                        b.u32(0).bytes(b"soun").zeros(12).bytes(b"SoundHandler\0");
                    });
                    d.boxed(b"minf", |n| {
                        n.full(b"smhd", 0, 0, |b| {
                            b.u16(0).u16(0);
                        });
                        n.boxed(b"dinf", |i| {
                            i.full(b"dref", 0, 0, |r| {
                                r.u32(1);
                                r.full(b"url ", 0, 1, |_| {});
                            });
                        });
                        n.boxed(b"stbl", |s| {
                            s.full(b"stsd", 0, 0, |b| {
                                b.u32(1);
                                b.boxed(&spec.codec, |e| {
                                    e.zeros(6).u16(1);
                                    e.u16(0).u16(0).u32(0);
                                    e.u16(spec.channels).u16(spec.sample_bits).u16(0).u16(0);
                                    // the 16.16 rate field only holds 16 bits of the integer part
                                    e.u32(if spec.sample_rate > 0xFFFF {
                                        0
                                    } else {
                                        spec.sample_rate << 16
                                    });
                                    e.bytes(spec.entry_extra);
                                });
                            });
                            // time to sample, run-length coded
                            let mut runs: Vec<(u32, u32)> = Vec::new();
                            for &(_, d) in spec.samples {
                                match runs.last_mut() {
                                    Some((n, rd)) if *rd == d => *n += 1,
                                    _ => runs.push((1, d)),
                                }
                            }
                            s.full(b"stts", 0, 0, |b| {
                                b.u32(runs.len() as u32);
                                for (n, d) in &runs {
                                    b.u32(*n).u32(*d);
                                }
                            });
                            s.full(b"stsc", 0, 0, |b| {
                                b.u32(1).u32(1).u32(spec.samples.len() as u32).u32(1);
                            });
                            s.full(b"stsz", 0, 0, |b| {
                                b.u32(0).u32(spec.samples.len() as u32);
                                for (sz, _) in spec.samples {
                                    b.u32(*sz);
                                }
                            });
                            s.full(b"stco", 0, 0, |b| {
                                b.u32(1).u32(chunk_offset);
                            });
                        });
                    });
                });
            });
            if let Some(ilst) = spec.ilst {
                m.boxed(b"udta", |u| {
                    u.full(b"meta", 0, 0, |b| {
                        b.full(b"hdlr", 0, 0, |h| {
                            h.u32(0).bytes(b"mdir").bytes(b"appl").zeros(8).u8(0);
                        });
                        b.boxed(b"ilst", |i| {
                            i.bytes(ilst);
                        });
                    });
                });
            }
        });
        w.buf
    };
    let mut head = BoxWriter::new();
    ftyp(&mut head);
    let moov_len = moov(0).len();
    let data_start = head.buf.len() + moov_len + 8;
    let mut out = head.buf;
    out.extend_from_slice(&moov(data_start as u32));
    out.extend_from_slice(&((payload.len() as u32 + 8).to_be_bytes()));
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn writer_output_parses_back() {
        let samples = [(5u32, 1024u32), (3, 1024), (4, 512)];
        let payload = b"aaaaabbbcccc";
        let spec = AudioTrackSpec {
            timescale: 44100,
            codec: *b"alac",
            channels: 2,
            sample_bits: 16,
            sample_rate: 44100,
            entry_extra: &[],
            samples: &samples,
            ilst: None,
            brand: *b"M4A ",
        };
        let file = write_audio(&spec, payload);
        assert!(sniff(&file));
        let m = parse(&file, &Limits::default()).unwrap();
        assert_eq!(&m.brand, b"M4A ");
        assert_eq!(m.tracks.len(), 1);
        let t = &m.tracks[0];
        assert_eq!(&t.handler, b"soun");
        assert_eq!(t.timescale, 44100);
        assert_eq!(t.samples.len(), 3);
        assert_eq!(sample_bytes(&file, &t.samples[1]).unwrap(), b"bbb");
        assert_eq!(sample_bytes(&file, &t.samples[2]).unwrap(), b"cccc");
        assert_eq!(t.samples[2].duration, 512);
    }
}
