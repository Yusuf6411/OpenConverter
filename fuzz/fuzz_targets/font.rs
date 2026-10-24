#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_core::Limits;
use oc_font::{build, convert, render, Font};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_alloc: 1 << 26, max_output: 1 << 26, ..Limits::default() };
    let sf = if build::is_woff(data) {
        match build::woff_to_sfnt(data, &limits) {
            Ok(v) => v,
            Err(_) => return,
        }
    } else {
        data.to_vec()
    };
    let Ok(font) = Font::parse(&sf) else { return };
    for gid in 0..font.num_glyphs.min(64) {
        let _ = oc_font::outline(&font, gid);
        let _ = font.advance(gid);
        let _ = font.glyph_name(gid);
    }
    let _ = font.unicode_map();
    let _ = font.name(1);
    let _ = font.glyph_for_char('A');
    let kerns = render::kern_pairs(&font);
    let _ = render::layout(&font, "Hello, wörld", &kerns);
    if font.num_glyphs <= 512 {
        if font.is_cff {
            let _ = convert::cff_to_truetype(&font, 0.5);
        } else {
            let _ = convert::truetype_to_cff(&font);
        }
        let _ = build::sfnt_to_woff(&sf, 6);
        let _ = render::specimen(&font, 400);
    }
});
