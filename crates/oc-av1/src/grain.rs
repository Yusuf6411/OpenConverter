//! Film grain synthesis (section 7.18.3).
use crate::decoder::Picture;
use crate::hdr::FilmGrain;
use crate::tables::GAUSSIAN_SEQUENCE;

fn round2(x: i32, n: u32) -> i32 {
    if n == 0 {
        x
    } else {
        (x + (1 << (n - 1))) >> n
    }
}

struct Rng(u32);

impl Rng {
    fn get(&mut self, bits: u32) -> u32 {
        let r = self.0;
        let bit = (r ^ (r >> 1) ^ (r >> 3) ^ (r >> 12)) & 1;
        let r = (r >> 1) | (bit << 15);
        self.0 = r;
        (r >> (16 - bits)) & ((1 << bits) - 1)
    }
}

fn build_lut(fg: &FilmGrain, plane: usize) -> [i32; 256] {
    let (n, xs, ys) = if plane == 0 || fg.chroma_scaling_from_luma {
        (fg.num_y_points, &fg.point_y_value, &fg.point_y_scaling)
    } else if plane == 1 {
        (fg.num_cb_points, &fg.point_cb_value, &fg.point_cb_scaling)
    } else {
        (fg.num_cr_points, &fg.point_cr_value, &fg.point_cr_scaling)
    };
    let mut lut = [0i32; 256];
    if n == 0 {
        return lut;
    }
    for x in 0..usize::from(xs[0]) {
        lut[x] = i32::from(ys[0]);
    }
    for i in 0..n - 1 {
        let dy = i32::from(ys[i + 1]) - i32::from(ys[i]);
        let dx = i32::from(xs[i + 1]) - i32::from(xs[i]);
        if dx <= 0 {
            continue;
        }
        let delta = dy * ((65536 + (dx >> 1)) / dx);
        for x in 0..dx {
            let v = i32::from(ys[i]) + ((x * delta + 32768) >> 16);
            lut[(i32::from(xs[i]) + x) as usize] = v;
        }
    }
    for x in usize::from(xs[n - 1])..256 {
        lut[x] = i32::from(ys[n - 1]);
    }
    lut
}

pub fn apply_film_grain(pic: &mut Picture, fg: &FilmGrain) {
    let bd = pic.bit_depth;
    let mono = pic.mono || pic.planes.len() < 3;
    let num_planes = if mono { 1 } else { 3 };
    let (sub_x, sub_y) = (pic.subsampling_x, pic.subsampling_y);
    let (w, h) = (pic.width, pic.height);
    let grain_center = 128i32 << (bd - 8);
    let grain_min = -grain_center;
    let grain_max = (256i32 << (bd - 8)) - 1 - grain_center;

    // Generate grain (7.18.3.3).
    let mut rng = Rng(fg.grain_seed);
    let mut luma = vec![[0i32; 82]; 73];
    let shift = 12 - bd + fg.grain_scale_shift;
    for row in luma.iter_mut() {
        for v in row.iter_mut() {
            let g = if fg.num_y_points > 0 {
                i32::from(GAUSSIAN_SEQUENCE[rng.get(11) as usize])
            } else {
                0
            };
            *v = round2(g, shift);
        }
    }
    let ar_shift = fg.ar_coeff_shift_minus_6 + 6;
    let lag = fg.ar_coeff_lag as i32;
    for y in 3..73usize {
        for x in 3..79usize {
            let mut s = 0;
            let mut pos = 0;
            'outer: for dr in -lag..=0 {
                for dc in -lag..=lag {
                    if dr == 0 && dc == 0 {
                        break 'outer;
                    }
                    let c = i32::from(fg.ar_coeffs_y_plus_128[pos]) - 128;
                    s += luma[(y as i32 + dr) as usize][(x as i32 + dc) as usize] * c;
                    pos += 1;
                }
            }
            luma[y][x] = (luma[y][x] + round2(s, ar_shift)).clamp(grain_min, grain_max);
        }
    }
    let chroma_w = if sub_x == 1 { 44 } else { 82 };
    let chroma_h = if sub_y == 1 { 38 } else { 73 };
    let mut cb = vec![[0i32; 82]; 73];
    let mut cr = vec![[0i32; 82]; 73];
    if !mono {
        rng.0 = fg.grain_seed ^ 0xb524;
        for y in 0..chroma_h {
            for x in 0..chroma_w {
                let g = if fg.num_cb_points > 0 || fg.chroma_scaling_from_luma {
                    i32::from(GAUSSIAN_SEQUENCE[rng.get(11) as usize])
                } else {
                    0
                };
                cb[y][x] = round2(g, shift);
            }
        }
        rng.0 = fg.grain_seed ^ 0x49d8;
        for y in 0..chroma_h {
            for x in 0..chroma_w {
                let g = if fg.num_cr_points > 0 || fg.chroma_scaling_from_luma {
                    i32::from(GAUSSIAN_SEQUENCE[rng.get(11) as usize])
                } else {
                    0
                };
                cr[y][x] = round2(g, shift);
            }
        }
        for y in 3..chroma_h {
            for x in 3..chroma_w - 3 {
                let (mut s0, mut s1) = (0, 0);
                let mut pos = 0;
                'o2: for dr in -lag..=0 {
                    for dc in -lag..=lag {
                        let c0 = i32::from(fg.ar_coeffs_cb_plus_128[pos]) - 128;
                        let c1 = i32::from(fg.ar_coeffs_cr_plus_128[pos]) - 128;
                        if dr == 0 && dc == 0 {
                            if fg.num_y_points > 0 {
                                let mut l = 0;
                                let lx = ((x - 3) << sub_x) + 3;
                                let ly = ((y - 3) << sub_y) + 3;
                                for i in 0..=sub_y {
                                    for j in 0..=sub_x {
                                        l += luma[ly + i][lx + j];
                                    }
                                }
                                l = round2(l, (sub_x + sub_y) as u32);
                                s0 += l * c0;
                                s1 += l * c1;
                            }
                            break 'o2;
                        }
                        let yy = (y as i32 + dr) as usize;
                        let xx = (x as i32 + dc) as usize;
                        s0 += cb[yy][xx] * c0;
                        s1 += cr[yy][xx] * c1;
                        pos += 1;
                    }
                }
                cb[y][x] = (cb[y][x] + round2(s0, ar_shift)).clamp(grain_min, grain_max);
                cr[y][x] = (cr[y][x] + round2(s1, ar_shift)).clamp(grain_min, grain_max);
            }
        }
    }

    let luts = [build_lut(fg, 0), build_lut(fg, 1), build_lut(fg, 2)];

    // Noise stripes (7.18.3.5).
    let stripe_w = w + 96;
    let n_stripes = ((h + 1) / 2).div_ceil(16);
    let mut stripes: Vec<[Vec<i32>; 3]> = Vec::with_capacity(n_stripes);
    let mut luma_num = 0u32;
    let mut y = 0;
    while y < (h + 1) / 2 {
        let mut st: [Vec<i32>; 3] = [
            vec![0; 34 * stripe_w],
            vec![0; 34 * stripe_w],
            vec![0; 34 * stripe_w],
        ];
        rng.0 = fg.grain_seed;
        rng.0 ^= ((luma_num * 37 + 178) & 255) << 8;
        rng.0 ^= (luma_num * 173 + 105) & 255;
        let mut x = 0;
        while x < (w + 1) / 2 {
            let rand = rng.get(8) as usize;
            let off_x = rand >> 4;
            let off_y = rand & 15;
            for plane in 0..num_planes {
                let psx = if plane > 0 { sub_x } else { 0 };
                let psy = if plane > 0 { sub_y } else { 0 };
                let pox = if psx == 1 { 6 + off_x } else { 9 + off_x * 2 };
                let poy = if psy == 1 { 6 + off_y } else { 9 + off_y * 2 };
                for i in 0..(34 >> psy) {
                    for j in 0..(34 >> psx) {
                        let mut g = match plane {
                            0 => luma[poy + i][pox + j],
                            1 => cb[poy + i][pox + j],
                            _ => cr[poy + i][pox + j],
                        };
                        if psx == 0 {
                            let idx = i * stripe_w + x * 2 + j;
                            if j < 2 && fg.overlap_flag && x > 0 {
                                let old = st[plane][idx];
                                g = if j == 0 {
                                    old * 27 + g * 17
                                } else {
                                    old * 17 + g * 27
                                };
                                g = round2(g, 5).clamp(grain_min, grain_max);
                            }
                            st[plane][idx] = g;
                        } else {
                            let idx = i * stripe_w + x + j;
                            if j == 0 && fg.overlap_flag && x > 0 {
                                let old = st[plane][idx];
                                g = round2(old * 23 + g * 22, 5).clamp(grain_min, grain_max);
                            }
                            st[plane][idx] = g;
                        }
                    }
                }
            }
            x += 16;
        }
        stripes.push(st);
        luma_num += 1;
        y += 16;
    }

    // Noise image.
    let mut noise: Vec<Vec<i32>> = Vec::new();
    for plane in 0..num_planes {
        let psx = if plane > 0 { sub_x } else { 0 };
        let psy = if plane > 0 { sub_y } else { 0 };
        let pw = (w + psx) >> psx;
        let ph = (h + psy) >> psy;
        let mut img = vec![0i32; pw * ph];
        for y in 0..ph {
            let ln = y >> (5 - psy);
            let i = y - (ln << (5 - psy));
            for x in 0..pw {
                let mut g = stripes[ln][plane][i * stripe_w + x];
                if psy == 0 {
                    if i < 2 && ln > 0 && fg.overlap_flag {
                        let old = stripes[ln - 1][plane][(i + 32) * stripe_w + x];
                        g = if i == 0 {
                            old * 27 + g * 17
                        } else {
                            old * 17 + g * 27
                        };
                        g = round2(g, 5).clamp(grain_min, grain_max);
                    }
                } else if i < 1 && ln > 0 && fg.overlap_flag {
                    let old = stripes[ln - 1][plane][(i + 16) * stripe_w + x];
                    g = round2(old * 23 + g * 22, 5).clamp(grain_min, grain_max);
                }
                img[y * pw + x] = g;
            }
        }
        noise.push(img);
    }

    // Blend.
    let (min_v, max_luma, max_chroma) = if fg.clip_to_restricted_range {
        let ml = 235i32 << (bd - 8);
        (
            16i32 << (bd - 8),
            ml,
            if pic.matrix_coefficients == 0 {
                ml
            } else {
                240i32 << (bd - 8)
            },
        )
    } else {
        let m = (256i32 << (bd - 8)) - 1;
        (0, m, m)
    };
    let scaling_shift = fg.grain_scaling_minus_8 + 8;
    let max_pix = (1i32 << bd) - 1;
    let scale_lut = |plane: usize, index: i32| -> i32 {
        let sh = bd - 8;
        let x = (index >> sh) as usize;
        let rem = index - ((x as i32) << sh);
        if bd == 8 || x == 255 {
            luts[plane][x]
        } else {
            let s = luts[plane][x];
            let e = luts[plane][x + 1];
            s + round2((e - s) * rem, sh)
        }
    };
    if !mono {
        let cw = (w + sub_x) >> sub_x;
        let ch = (h + sub_y) >> sub_y;
        let (y_plane, rest) = pic.planes.split_at_mut(1);
        let (u_plane, v_plane) = rest.split_at_mut(1);
        let y_plane = &y_plane[0];
        for y in 0..ch {
            for x in 0..cw {
                let lx = x << sub_x;
                let ly = y << sub_y;
                let lnx = (lx + 1).min(w - 1);
                let avg = if sub_x == 1 {
                    round2(
                        i32::from(y_plane[ly * w + lx]) + i32::from(y_plane[ly * w + lnx]),
                        1,
                    )
                } else {
                    i32::from(y_plane[ly * w + lx])
                };
                for (pi, buf) in [(1usize, &mut u_plane[0]), (2usize, &mut v_plane[0])] {
                    let (np, lm, m, off) = if pi == 1 {
                        (fg.num_cb_points, fg.cb_luma_mult, fg.cb_mult, fg.cb_offset)
                    } else {
                        (fg.num_cr_points, fg.cr_luma_mult, fg.cr_mult, fg.cr_offset)
                    };
                    if np > 0 || fg.chroma_scaling_from_luma {
                        let orig = i32::from(buf[y * cw + x]);
                        let merged = if fg.chroma_scaling_from_luma {
                            avg
                        } else {
                            let combined = avg * (lm as i32 - 128) + orig * (m as i32 - 128);
                            ((combined >> 6) + ((off as i32 - 256) << (bd - 8))).clamp(0, max_pix)
                        };
                        let n = noise[pi][y * cw + x];
                        let n = round2(scale_lut(pi, merged) * n, scaling_shift);
                        buf[y * cw + x] = (orig + n).clamp(min_v, max_chroma) as u16;
                    }
                }
            }
        }
    }
    if fg.num_y_points > 0 {
        let yp = &mut pic.planes[0];
        for y in 0..h {
            for x in 0..w {
                let orig = i32::from(yp[y * w + x]);
                let n = round2(scale_lut(0, orig) * noise[0][y * w + x], scaling_shift);
                yp[y * w + x] = (orig + n).clamp(min_v, max_luma) as u16;
            }
        }
    }
}
