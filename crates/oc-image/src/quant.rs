//! Colour quantisation for palette formats (GIF, 8-bit PNG, ICO): exact palettes when the
//! image has few enough colours, otherwise median cut with optional Floyd-Steinberg dithering.
//!
//! Pixels with alpha below 128 are transparent and get a dedicated palette index.
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quantized {
    /// Opaque colours. The transparent index, when there is one, follows them.
    pub palette: Vec<[u8; 3]>,
    pub indices: Vec<u8>,
    pub transparent: Option<u8>,
    /// True when the palette holds every colour of the image, so nothing was lost
    /// (apart from partial alpha, which `partial_alpha` reports).
    pub exact: bool,
    /// Some pixels were neither fully opaque nor fully transparent and were snapped.
    pub partial_alpha: bool,
}

type Entry = ([u8; 3], u32);

struct Cut {
    entries: Vec<Entry>,
}

impl Cut {
    fn range(&self) -> (usize, u8) {
        let mut best = (0usize, 0u8);
        for ch in 0..3 {
            let (lo, hi) = self.entries.iter().fold((255u8, 0u8), |(lo, hi), (c, _)| {
                let v = c.get(ch).copied().unwrap_or(0);
                (lo.min(v), hi.max(v))
            });
            if hi - lo >= best.1 && (hi - lo > best.1 || ch == 0) {
                best = (ch, hi - lo);
            }
        }
        best
    }

    fn average(&self) -> [u8; 3] {
        let total: u64 = self.entries.iter().map(|(_, n)| u64::from(*n)).sum();
        let total = total.max(1);
        let mut out = [0u8; 3];
        for (ch, o) in out.iter_mut().enumerate() {
            let sum: u64 = self
                .entries
                .iter()
                .map(|(c, n)| u64::from(c.get(ch).copied().unwrap_or(0)) * u64::from(*n))
                .sum();
            *o = ((sum + total / 2) / total) as u8;
        }
        out
    }
}

fn median_cut(mut entries: Vec<Entry>, target: usize) -> Vec<[u8; 3]> {
    entries.sort_unstable();
    let mut boxes = vec![Cut { entries }];
    while boxes.len() < target {
        // Split the box that spans the widest channel range and has colours to separate.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.entries.len() > 1)
            .max_by_key(|(i, b)| (b.range().1, std::cmp::Reverse(*i)))
            .map(|(i, _)| i);
        let Some(i) = pick else { break };
        let b = boxes.swap_remove(i);
        let (ch, _) = b.range();
        let mut e = b.entries;
        e.sort_by_key(|(c, _)| (c.get(ch).copied().unwrap_or(0), *c));
        let half: u64 = e.iter().map(|(_, n)| u64::from(*n)).sum::<u64>() / 2;
        let mut acc = 0u64;
        let mut cut = 1;
        for (k, (_, n)) in e.iter().enumerate() {
            acc += u64::from(*n);
            if acc >= half {
                cut = (k + 1).clamp(1, e.len() - 1);
                break;
            }
        }
        let right = e.split_off(cut);
        boxes.push(Cut { entries: e });
        boxes.push(Cut { entries: right });
    }
    let mut palette: Vec<[u8; 3]> = boxes.iter().map(Cut::average).collect();
    palette.sort_unstable();
    palette.dedup();
    palette
}

fn nearest(palette: &[[u8; 3]], c: [i32; 3]) -> u8 {
    let mut best = (i64::MAX, 0usize);
    for (i, p) in palette.iter().enumerate() {
        let d: i64 = p
            .iter()
            .zip(c)
            .map(|(&a, b)| {
                let d = i64::from(a) - i64::from(b);
                d * d
            })
            .sum();
        if d < best.0 {
            best = (d, i);
        }
    }
    best.1 as u8
}

/// Quantise tightly packed 8-bit RGBA pixels to at most 256 palette entries.
pub fn quantize(rgba: &[u8], width: usize, height: usize, dither: bool) -> Quantized {
    let px: Vec<[u8; 4]> = rgba.as_chunks::<4>().0.to_vec();
    let mut hist: HashMap<[u8; 3], u32> = HashMap::new();
    let mut has_transparent = false;
    let mut partial_alpha = false;
    for &[r, g, b, a] in &px {
        if a < 128 {
            has_transparent = true;
        } else {
            *hist.entry([r, g, b]).or_insert(0) += 1;
        }
        if a != 0 && a != 255 {
            partial_alpha = true;
        }
    }
    let budget = 256 - usize::from(has_transparent);
    let exact = hist.len() <= budget;
    let palette: Vec<[u8; 3]> = if exact {
        let mut p: Vec<[u8; 3]> = hist.keys().copied().collect();
        p.sort_unstable();
        p
    } else {
        median_cut(hist.into_iter().collect(), budget)
    };
    let transparent = has_transparent.then_some(palette.len() as u8);
    // `palette.len()` can be 256 only when there is no transparent index.
    let mut indices = vec![0u8; px.len()];
    if exact {
        let lookup: HashMap<[u8; 3], u8> = palette
            .iter()
            .enumerate()
            .map(|(i, &c)| (c, i as u8))
            .collect();
        for (o, &[r, g, b, a]) in indices.iter_mut().zip(&px) {
            *o = if a < 128 {
                transparent.unwrap_or(0)
            } else {
                lookup.get(&[r, g, b]).copied().unwrap_or(0)
            };
        }
    } else if dither {
        // Floyd-Steinberg, serpentine-free for determinism. Errors in 1/16 units.
        let mut err = vec![[0i32; 3]; width * height];
        for y in 0..height {
            for x in 0..width {
                let i = y * width + x;
                let Some(&[r, g, b, a]) = px.get(i) else {
                    continue;
                };
                if a < 128 {
                    if let Some(o) = indices.get_mut(i) {
                        *o = transparent.unwrap_or(0);
                    }
                    continue;
                }
                let e = err.get(i).copied().unwrap_or([0; 3]);
                let want = [
                    (i32::from(r) + e[0] / 16).clamp(0, 255),
                    (i32::from(g) + e[1] / 16).clamp(0, 255),
                    (i32::from(b) + e[2] / 16).clamp(0, 255),
                ];
                let k = nearest(&palette, want);
                if let Some(o) = indices.get_mut(i) {
                    *o = k;
                }
                let got = palette.get(usize::from(k)).copied().unwrap_or([0; 3]);
                let diff = [
                    want[0] - i32::from(got[0]),
                    want[1] - i32::from(got[1]),
                    want[2] - i32::from(got[2]),
                ];
                let mut spread = |dx: isize, dy: usize, w: i32| {
                    let nx = x as isize + dx;
                    if nx < 0 || nx as usize >= width || y + dy >= height {
                        return;
                    }
                    if let Some(t) = err.get_mut((y + dy) * width + nx as usize) {
                        for (tc, d) in t.iter_mut().zip(diff) {
                            *tc += d * w;
                        }
                    }
                };
                spread(1, 0, 7);
                spread(-1, 1, 3);
                spread(0, 1, 5);
                spread(1, 1, 1);
            }
        }
    } else {
        let mut cache: HashMap<[u8; 3], u8> = HashMap::new();
        for (o, &[r, g, b, a]) in indices.iter_mut().zip(&px) {
            *o = if a < 128 {
                transparent.unwrap_or(0)
            } else {
                *cache.entry([r, g, b]).or_insert_with(|| {
                    nearest(&palette, [i32::from(r), i32::from(g), i32::from(b)])
                })
            };
        }
    }
    Quantized {
        palette,
        indices,
        transparent,
        exact,
        partial_alpha,
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn gradient(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            v.extend_from_slice(&[(i * 7) as u8, ((i * 13) >> 1) as u8, (i * 3) as u8, 255]);
        }
        v
    }

    #[test]
    fn few_colours_are_exact() {
        let mut px = gradient(10);
        px.extend_from_slice(&[1, 2, 3, 0]); // transparent
        let q = quantize(&px, 11, 1, true);
        assert!(q.exact);
        assert_eq!(q.palette.len(), 10);
        assert_eq!(q.transparent, Some(10));
        assert_eq!(q.indices.last(), Some(&10));
        for (i, &idx) in q.indices.iter().take(10).enumerate() {
            let c = q.palette[usize::from(idx)];
            assert_eq!(
                [c[0], c[1], c[2]],
                [px[i * 4], px[i * 4 + 1], px[i * 4 + 2]]
            );
        }
    }

    #[test]
    fn many_colours_fit_the_budget_and_stay_close() {
        let px = gradient(5000);
        for dither in [false, true] {
            let q = quantize(&px, 5000, 1, dither);
            assert!(!q.exact);
            assert!(q.palette.len() <= 256);
            // Mean per-channel error is small for a smooth ramp.
            let mut total = 0u64;
            for (i, &idx) in q.indices.iter().enumerate() {
                let c = q.palette[usize::from(idx)];
                for ch in 0..3 {
                    total += u64::from(c[ch].abs_diff(px[i * 4 + ch]));
                }
            }
            assert!(
                total / (5000 * 3) < 12,
                "dither={dither} mean err {}",
                total / 15000
            );
        }
    }

    #[test]
    fn budget_leaves_room_for_transparency() {
        let mut px = gradient(3000);
        px.extend_from_slice(&[0, 0, 0, 0]);
        let q = quantize(&px, 3001, 1, false);
        assert!(q.palette.len() <= 255);
        assert_eq!(q.transparent, Some(q.palette.len() as u8));
    }

    #[test]
    fn partial_alpha_is_reported() {
        let q = quantize(&[10, 20, 30, 100, 1, 1, 1, 255], 2, 1, false);
        assert!(q.partial_alpha);
        assert_eq!(q.transparent, Some(1));
    }
}
