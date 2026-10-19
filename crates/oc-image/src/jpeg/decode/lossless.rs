//! Lossless JPEG scans (ITU T.81 Annex H, SOF3): differential pulse-code modulation with
//! one of seven predictors, Huffman-coded differences and an optional point transform.
//! Every component is sampled once per pixel here; subsampled lossless files are refused
//! when the frame header is read.
use super::{Bits, Frame, Scan, Tables};
use oc_core::{Error, Progress, Result};

/// The predicted value for the sample at (`x`, `y`) given its neighbours.
fn predict(selector: usize, ra: i32, rb: i32, rc: i32) -> i32 {
    match selector {
        1 => ra,
        2 => rb,
        3 => rc,
        4 => ra + rb - rc,
        5 => ra + ((rb - rc) >> 1),
        6 => rb + ((ra - rc) >> 1),
        _ => (ra + rb) >> 1,
    }
}

pub(super) fn decode_scan(
    frame: &mut Frame,
    scan: &Scan,
    bits: &mut Bits<'_>,
    tables: &Tables,
    restart_interval: usize,
    progress: &dyn Progress,
) -> Result<()> {
    let (w, h) = (frame.width, frame.height);
    let (precision, pt, selector) = (frame.precision, scan.al, scan.ss);
    let default = 1i32 << (precision - pt - 1);
    let mut huffs = Vec::with_capacity(scan.comps.len());
    for sc in &scan.comps {
        huffs.push(
            tables
                .dc
                .get(sc.td)
                .and_then(Option::as_ref)
                .ok_or_else(|| Error::corrupt("JPEG scan uses an undefined Huffman table"))?,
        );
    }
    // The pixel where the current restart interval began; its row is predicted from the left.
    let mut interval_start = 0usize;
    for pixel in 0..w * h {
        if restart_interval > 0 && pixel > 0 && pixel % restart_interval == 0 {
            bits.restart();
            interval_start = pixel;
        }
        if pixel % 4096 == 0 {
            progress.check_cancelled()?;
        }
        let (x, y) = (pixel % w, pixel / w);
        for (sc, huff) in scan.comps.iter().zip(&huffs) {
            let s = bits.symbol(huff)?;
            let diff = match s {
                0..=15 => bits.extend(u32::from(s)),
                16 => 32768,
                _ => {
                    return Err(Error::corrupt(
                        "JPEG lossless difference category too large",
                    ))
                }
            };
            let comp = frame
                .comps
                .get_mut(sc.index)
                .ok_or_else(|| Error::corrupt("JPEG scan names a missing component"))?;
            let at = |dx: usize, dy: usize| {
                comp.samples
                    .get((y - dy) * w + (x - dx))
                    .copied()
                    .map_or(0, i32::from)
            };
            let pred = if pixel == interval_start {
                default
            } else if y == interval_start / w {
                at(1, 0)
            } else if x == 0 {
                at(0, 1)
            } else {
                predict(selector, at(1, 0), at(0, 1), at(1, 1))
            };
            if let Some(o) = comp.samples.get_mut(pixel) {
                *o = ((pred + diff) & 0xFFFF) as u16;
            }
        }
    }
    if pt > 0 {
        for sc in &scan.comps {
            if let Some(c) = frame.comps.get_mut(sc.index) {
                for v in &mut c.samples {
                    *v <<= pt;
                }
            }
        }
    }
    Ok(())
}
