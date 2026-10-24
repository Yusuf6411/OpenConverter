//! Image XObjects and inline images to straight RGBA.
use crate::color::ColorSpace;
use crate::doc::Pdf;
use crate::filter::{decode_stream, ImageFilter};
use crate::object::{Dict, Obj, Stream};
use crate::ops::ImageData;
use oc_core::{Limits, NoProgress};

fn normalize(d: &Dict) -> Dict {
    let map = [
        ("W", "Width"),
        ("H", "Height"),
        ("BPC", "BitsPerComponent"),
        ("CS", "ColorSpace"),
        ("D", "Decode"),
        ("IM", "ImageMask"),
        ("I", "Interpolate"),
        ("F", "Filter"),
        ("DP", "DecodeParms"),
    ];
    let mut out = d.clone();
    for (short, long) in map {
        if let Some(v) = d.get(short) {
            out.entry(long.to_string()).or_insert_with(|| v.clone());
        }
    }
    out
}

fn unpack(data: &[u8], w: usize, h: usize, n: usize, bpc: usize) -> Vec<u16> {
    let row_bytes = (w * n * bpc).div_ceil(8);
    let mut out = Vec::with_capacity(w * h * n);
    for y in 0..h {
        let row = data.get(y * row_bytes..).unwrap_or_default();
        let row = row.get(..row_bytes.min(row.len())).unwrap_or_default();
        for i in 0..w * n {
            let v = match bpc {
                8 => u16::from(row.get(i).copied().unwrap_or(0)),
                16 => {
                    u16::from(row.get(2 * i).copied().unwrap_or(0)) << 8
                        | u16::from(row.get(2 * i + 1).copied().unwrap_or(0))
                }
                _ => {
                    let bit = i * bpc;
                    let byte = row.get(bit / 8).copied().unwrap_or(0);
                    let shift = 8 - bpc - bit % 8;
                    u16::from((byte >> shift) & ((1u16 << bpc) - 1) as u8)
                }
            };
            out.push(v);
        }
    }
    out
}

/// Raw samples of an image: `(samples, bits per component, components)`.
struct Samples {
    data: Vec<u16>,
    bpc: usize,
    n: usize,
}

fn jpeg_samples(data: &[u8]) -> Option<(Vec<u8>, usize, usize, usize)> {
    let (img, _) = oc_image::jpeg::decode(data, &Limits::default(), &NoProgress).ok()?;
    let (img, _) = oc_image::ops::convert_depth(&img, oc_image::Depth::Eight);
    let n = img.layout.channels();
    let (w, h) = (img.width as usize, img.height as usize);
    match img.layout {
        oc_image::Layout::Gray | oc_image::Layout::Rgb => Some((img.data, w, h, n)),
        _ => {
            let (img2, _) =
                oc_image::ops::convert_layout(&img, oc_image::Layout::Rgb, [255, 255, 255]);
            Some((img2.data, w, h, 3))
        }
    }
}

pub fn decode_image(
    pdf: &Pdf,
    s: &Stream,
    res: &Dict,
    fill: [f32; 3],
    notes: &mut Vec<String>,
    depth: u32,
) -> Option<ImageData> {
    if depth > 3 {
        return None;
    }
    let d = normalize(&s.dict);
    let w = pdf.dget_i64(&d, "Width")?.clamp(1, 1 << 16) as usize;
    let h = pdf.dget_i64(&d, "Height")?.clamp(1, 1 << 16) as usize;
    if w.checked_mul(h)? > 1 << 28 {
        notes.push("an image is too large and was skipped".into());
        return None;
    }
    let is_mask = pdf.dget(&d, "ImageMask").as_bool().unwrap_or(false);
    let interpolate = pdf.dget(&d, "Interpolate").as_bool().unwrap_or(false);
    let mut bpc = pdf
        .dget_i64(&d, "BitsPerComponent")
        .unwrap_or(8)
        .clamp(1, 16) as usize;
    let (data, imgf) = match decode_stream(pdf, s, true) {
        Ok(v) => v,
        Err(e) => {
            notes.push(format!("an image could not be decoded: {e}"));
            return None;
        }
    };
    let decode_arr = pdf.dget_numbers(&d, "Decode");
    let mut cs = if is_mask {
        Some(ColorSpace::Gray)
    } else {
        d.get("ColorSpace")
            .and_then(|c| ColorSpace::parse(pdf, c, res, 0))
    };

    let (samples, n): (Samples, usize);
    match imgf {
        Some(ImageFilter { name, parms }) => {
            match name.as_str() {
                "DCTDecode" => {
                    let (px, jw, jh, jn) = jpeg_samples(&data)?;
                    if (jw, jh) != (w, h) {
                        notes.push("a JPEG image's size differs from its dictionary; the JPEG size was used".into());
                    }
                    let n = jn;
                    if cs.as_ref().is_none_or(|c| {
                        c.n_components() != jn && !matches!(c, ColorSpace::Indexed { .. })
                    }) {
                        cs = Some(if jn == 1 {
                            ColorSpace::Gray
                        } else {
                            ColorSpace::Rgb
                        });
                    }
                    let samples = Samples {
                        data: px.iter().map(|b| u16::from(*b)).collect(),
                        bpc: 8,
                        n,
                    };
                    return finish(
                        pdf,
                        &d,
                        res,
                        cs?,
                        samples,
                        jw,
                        jh,
                        is_mask,
                        fill,
                        decode_arr,
                        interpolate,
                        notes,
                        depth,
                    );
                }
                "JPXDecode" => {
                    let Ok((img, _)) = oc_image::jp2::decode(&data, &Limits::default()) else {
                        notes.push("a JPEG 2000 image could not be decoded".into());
                        return None;
                    };
                    let (img, _) = oc_image::ops::convert_depth(&img, oc_image::Depth::Eight);
                    // an alpha channel is dropped: PDF carries transparency in /SMask
                    let (jw, jh) = (img.width as usize, img.height as usize);
                    let (px, jn) = match img.layout {
                        oc_image::Layout::Gray | oc_image::Layout::Rgb => {
                            (img.data.clone(), img.layout.channels())
                        }
                        oc_image::Layout::GrayAlpha => {
                            (img.data.chunks_exact(2).map(|c| c[0]).collect(), 1)
                        }
                        oc_image::Layout::Rgba => (
                            img.data
                                .chunks_exact(4)
                                .flat_map(|c| [c[0], c[1], c[2]])
                                .collect(),
                            3,
                        ),
                    };
                    if cs.as_ref().is_none_or(|c| {
                        c.n_components() != jn && !matches!(c, ColorSpace::Indexed { .. })
                    }) {
                        cs = Some(if jn == 1 {
                            ColorSpace::Gray
                        } else {
                            ColorSpace::Rgb
                        });
                    }
                    let samples = Samples {
                        data: px.iter().map(|b| u16::from(*b)).collect(),
                        bpc: 8,
                        n: jn,
                    };
                    return finish(
                        pdf,
                        &d,
                        res,
                        cs?,
                        samples,
                        jw,
                        jh,
                        is_mask,
                        fill,
                        decode_arr,
                        interpolate,
                        notes,
                        depth,
                    );
                }
                "JBIG2Decode" => {
                    let globals = match pdf.dget(&parms, "JBIG2Globals") {
                        Obj::Stream(gs) => decode_stream(pdf, &gs, false).ok().map(|(b, _)| b),
                        _ => None,
                    };
                    let Ok((img, _)) = oc_image::jbig2::decode_embedded(
                        globals.as_deref(),
                        &data,
                        &Limits::default(),
                    ) else {
                        notes.push("a JBIG2 image could not be decoded".into());
                        return None;
                    };
                    let (jw, jh) = (img.width as usize, img.height as usize);
                    // decoded black is 0 here, which is what DeviceGray and /Decode expect
                    let samples = Samples {
                        data: img.data.iter().map(|v| u16::from(*v >= 128)).collect(),
                        bpc: 1,
                        n: 1,
                    };
                    if cs.is_none() {
                        cs = Some(ColorSpace::Gray);
                    }
                    return finish(
                        pdf,
                        &d,
                        res,
                        cs?,
                        samples,
                        jw,
                        jh,
                        is_mask,
                        fill,
                        decode_arr,
                        interpolate,
                        notes,
                        depth,
                    );
                }
                "CCITTFaxDecode" => {
                    let k = pdf.dget_i64(&parms, "K").unwrap_or(0);
                    let cols = pdf
                        .dget_i64(&parms, "Columns")
                        .unwrap_or(1728)
                        .clamp(1, 1 << 16) as usize;
                    let rows = pdf
                        .dget_i64(&parms, "Rows")
                        .filter(|r| *r > 0)
                        .map_or(h, |r| r as usize)
                        .max(h);
                    let black_is_1 = pdf.dget(&parms, "BlackIs1").as_bool().unwrap_or(false);
                    let mode = if k < 0 {
                        oc_image::ccitt::Mode::Group4
                    } else {
                        oc_image::ccitt::Mode::Group3 { two_d: k > 0 }
                    };
                    let (bits, _) = oc_image::ccitt::decode(&data, cols, rows, mode);
                    // decoder output: 1 = black; PDF default wants 0 = black
                    let packed: Vec<u8> = bits
                        .iter()
                        .map(|b| if black_is_1 { *b } else { !*b })
                        .collect();
                    if cs.is_none() {
                        cs = Some(ColorSpace::Gray);
                    }
                    let samples = Samples {
                        data: unpack(&packed, cols, h, 1, 1),
                        bpc: 1,
                        n: 1,
                    };
                    return finish(
                        pdf,
                        &d,
                        res,
                        cs?,
                        samples,
                        cols.min(w).max(1).max(cols),
                        h,
                        is_mask,
                        fill,
                        decode_arr,
                        interpolate,
                        notes,
                        depth,
                    );
                }
                other => {
                    let msg =
                        format!("an image uses {other}, which is not supported; it was skipped");
                    if !notes.contains(&msg) {
                        notes.push(msg);
                    }
                    return None;
                }
            }
        }
        None => {
            let cs_ref = cs.as_ref()?;
            n = if is_mask {
                1
            } else {
                cs_ref.n_components().max(1)
            };
            if is_mask {
                bpc = 1;
            }
            samples = Samples {
                data: unpack(&data, w, h, n, bpc),
                bpc,
                n,
            };
        }
    }
    finish(
        pdf,
        &d,
        res,
        cs?,
        samples,
        w,
        h,
        is_mask,
        fill,
        decode_arr,
        interpolate,
        notes,
        depth,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish(
    pdf: &Pdf,
    d: &Dict,
    res: &Dict,
    cs: ColorSpace,
    samples: Samples,
    w: usize,
    h: usize,
    is_mask: bool,
    fill: [f32; 3],
    decode_arr: Option<Vec<f64>>,
    interpolate: bool,
    notes: &mut Vec<String>,
    depth: u32,
) -> Option<ImageData> {
    let Samples { data, bpc, n } = samples;
    let maxv = ((1u32 << bpc) - 1) as f64;
    let mut rgba = vec![255u8; w * h * 4];
    if is_mask {
        let invert = decode_arr
            .as_ref()
            .is_some_and(|a| a.first().copied().unwrap_or(0.0) >= 1.0);
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            let v = data.get(i).copied().unwrap_or(1);
            let painted = (v == 0) != invert;
            px[0] = (fill[0] * 255.0 + 0.5) as u8;
            px[1] = (fill[1] * 255.0 + 0.5) as u8;
            px[2] = (fill[2] * 255.0 + 0.5) as u8;
            px[3] = if painted { 255 } else { 0 };
        }
        return Some(ImageData {
            width: w,
            height: h,
            rgba,
            stencil: true,
            interpolate,
        });
    }
    let dec = decode_arr
        .filter(|a| a.len() >= 2 * n)
        .unwrap_or_else(|| cs.default_decode(bpc));
    let default_decode = dec == cs.default_decode(bpc);
    // colour key mask
    let key: Option<Vec<u16>> = match pdf.dget(d, "Mask") {
        Obj::Array(a) => Some(
            a.iter()
                .filter_map(|o| pdf.resolve(o).as_i64().map(|v| v.clamp(0, 65535) as u16))
                .collect(),
        ),
        _ => None,
    };
    // lookup table for single-component images
    let lut: Option<Vec<[f32; 3]>> = (n == 1 && bpc <= 8).then(|| {
        (0..=(maxv as usize))
            .map(|v| cs.to_rgb(&[dec[0] + (v as f64) * (dec[1] - dec[0]) / maxv]))
            .collect()
    });
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut comps = vec![0.0f64; n];
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        let raw = data.get(i * n..i * n + n).unwrap_or_default();
        if raw.len() < n {
            continue;
        }
        let rgb = if let Some(l) = &lut {
            l.get(usize::from(raw[0])).copied().unwrap_or([0.0; 3])
        } else {
            for k in 0..n {
                let v = f64::from(raw[k]) / maxv;
                comps[k] = if default_decode
                    && matches!(cs, ColorSpace::Gray | ColorSpace::Rgb | ColorSpace::Cmyk)
                {
                    v
                } else {
                    dec[2 * k] + v * (dec[2 * k + 1] - dec[2 * k])
                };
            }
            cs.to_rgb(&comps)
        };
        px[0] = q(rgb[0]);
        px[1] = q(rgb[1]);
        px[2] = q(rgb[2]);
        if let Some(k) = &key {
            if k.len() >= 2 * n && (0..n).all(|c| raw[c] >= k[2 * c] && raw[c] <= k[2 * c + 1]) {
                px[3] = 0;
            }
        }
    }
    // soft mask or explicit mask
    let mut apply_alpha = |mask: &ImageData, from_stencil: bool| {
        for y in 0..h {
            for x in 0..w {
                let mx = (x * mask.width / w).min(mask.width - 1);
                let my = (y * mask.height / h).min(mask.height - 1);
                let a = if from_stencil {
                    // explicit mask: painted (opaque in the mask image) = hidden pixels
                    255 - mask
                        .rgba
                        .get((my * mask.width + mx) * 4 + 3)
                        .copied()
                        .unwrap_or(0)
                } else {
                    mask.rgba
                        .get((my * mask.width + mx) * 4)
                        .copied()
                        .unwrap_or(255)
                };
                if let Some(p) = rgba.get_mut((y * w + x) * 4 + 3) {
                    *p = ((u16::from(*p) * u16::from(a)) / 255) as u8;
                }
            }
        }
    };
    match pdf.dget(d, "SMask") {
        Obj::Stream(sm) => {
            let mut sd = sm.dict.clone();
            sd.insert("ColorSpace".into(), Obj::Name("DeviceGray".into()));
            let sm2 = Stream {
                dict: sd,
                raw: sm.raw.clone(),
                id: sm.id,
            };
            if let Some(mask) = decode_image(pdf, &sm2, res, [0.0; 3], notes, depth + 1) {
                apply_alpha(&mask, false);
            }
        }
        _ => {
            if let Obj::Stream(ms) = pdf.dget(d, "Mask") {
                let mut md = ms.dict.clone();
                md.insert("ImageMask".into(), Obj::Bool(true));
                let ms2 = Stream {
                    dict: md,
                    raw: ms.raw.clone(),
                    id: ms.id,
                };
                if let Some(mask) = decode_image(pdf, &ms2, res, [0.0; 3], notes, depth + 1) {
                    apply_alpha(&mask, true);
                }
            }
        }
    }
    Some(ImageData {
        width: w,
        height: h,
        rgba,
        stencil: false,
        interpolate,
    })
}
