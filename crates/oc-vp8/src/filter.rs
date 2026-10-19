//! The in-loop deblocking filters of VP8 (RFC 6386 section 15), applied to a whole frame after
//! reconstruction because intra prediction reads unfiltered pixels.
#![allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use crate::transform::clip8;

/// Per-macroblock filter parameters.
#[derive(Clone, Copy, Default, Debug)]
pub struct FilterInfo {
    pub limit: u8,
    pub ilevel: u8,
    pub hev_thresh: u8,
    pub inner: bool,
}

fn sclip1(v: i32) -> i32 {
    v.clamp(-128, 127)
}

fn sclip2(v: i32) -> i32 {
    v.clamp(-16, 15)
}

/// Pixels along the filtered edge are `step` apart across it and `along` apart down it.
struct Edge<'a> {
    px: &'a mut [u8],
    step: usize,
}

impl Edge<'_> {
    fn at(&self, p: usize, k: isize) -> i32 {
        i32::from(self.px[(p as isize + k * self.step as isize) as usize])
    }

    fn put(&mut self, p: usize, k: isize, v: i32) {
        self.px[(p as isize + k * self.step as isize) as usize] = clip8(v);
    }

    fn filter2(&mut self, p: usize) {
        let (p1, p0, q0, q1) = (self.at(p, -2), self.at(p, -1), self.at(p, 0), self.at(p, 1));
        let a = 3 * (q0 - p0) + sclip1(p1 - q1);
        let a1 = sclip2((a + 4) >> 3);
        let a2 = sclip2((a + 3) >> 3);
        self.put(p, -1, p0 + a2);
        self.put(p, 0, q0 - a1);
    }

    fn filter4(&mut self, p: usize) {
        let (p1, p0, q0, q1) = (self.at(p, -2), self.at(p, -1), self.at(p, 0), self.at(p, 1));
        let a = 3 * (q0 - p0);
        let a1 = sclip2((a + 4) >> 3);
        let a2 = sclip2((a + 3) >> 3);
        let a3 = (a1 + 1) >> 1;
        self.put(p, -2, p1 + a3);
        self.put(p, -1, p0 + a2);
        self.put(p, 0, q0 - a1);
        self.put(p, 1, q1 - a3);
    }

    fn filter6(&mut self, p: usize) {
        let (p2, p1, p0) = (self.at(p, -3), self.at(p, -2), self.at(p, -1));
        let (q0, q1, q2) = (self.at(p, 0), self.at(p, 1), self.at(p, 2));
        let a = sclip1(3 * (q0 - p0) + sclip1(p1 - q1));
        let a1 = (27 * a + 63) >> 7;
        let a2 = (18 * a + 63) >> 7;
        let a3 = (9 * a + 63) >> 7;
        self.put(p, -3, p2 + a3);
        self.put(p, -2, p1 + a2);
        self.put(p, -1, p0 + a1);
        self.put(p, 0, q0 - a1);
        self.put(p, 1, q1 - a2);
        self.put(p, 2, q2 - a3);
    }

    fn hev(&self, p: usize, thresh: i32) -> bool {
        (self.at(p, -2) - self.at(p, -1)).abs() > thresh
            || (self.at(p, 1) - self.at(p, 0)).abs() > thresh
    }

    fn needs_filter(&self, p: usize, t: i32) -> bool {
        let (p1, p0, q0, q1) = (self.at(p, -2), self.at(p, -1), self.at(p, 0), self.at(p, 1));
        4 * (p0 - q0).abs() + (p1 - q1).abs() <= t
    }

    fn needs_filter2(&self, p: usize, t: i32, it: i32) -> bool {
        if !self.needs_filter(p, t) {
            return false;
        }
        let g = |k| self.at(p, k);
        (g(-4) - g(-3)).abs() <= it
            && (g(-3) - g(-2)).abs() <= it
            && (g(-2) - g(-1)).abs() <= it
            && (g(3) - g(2)).abs() <= it
            && (g(2) - g(1)).abs() <= it
            && (g(1) - g(0)).abs() <= it
    }
}

/// Filters `size` positions starting at `start`, spaced `along` apart, across an edge whose
/// samples are `step` apart. `mb_edge` selects the wider macroblock-edge filter.
#[allow(clippy::too_many_arguments)]
fn loop_filter(
    px: &mut [u8],
    start: usize,
    step: usize,
    along: usize,
    size: usize,
    thresh: i32,
    ithresh: i32,
    hev_thresh: i32,
    mb_edge: bool,
) {
    let thresh2 = 2 * thresh + 1;
    let mut e = Edge { px, step };
    for n in 0..size {
        let p = start + n * along;
        if e.needs_filter2(p, thresh2, ithresh) {
            if e.hev(p, hev_thresh) {
                e.filter2(p);
            } else if mb_edge {
                e.filter6(p);
            } else {
                e.filter4(p);
            }
        }
    }
}

fn simple_filter(px: &mut [u8], start: usize, step: usize, along: usize, thresh: i32) {
    let thresh2 = 2 * thresh + 1;
    let mut e = Edge { px, step };
    for n in 0..16 {
        let p = start + n * along;
        if e.needs_filter(p, thresh2) {
            e.filter2(p);
        }
    }
}

/// One plane with the geometry the filters need.
pub struct PlaneMut<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
    /// Index of pixel (0, 0).
    pub origin: usize,
}

impl PlaneMut<'_> {
    fn at(&self, x: usize, y: usize) -> usize {
        self.origin + y * self.stride + x
    }
}

/// Filters the macroblock at (`mb_x`, `mb_y`) in raster order. `simple` selects the luma-only
/// simple filter.
pub fn filter_mb(
    simple: bool,
    info: FilterInfo,
    mb_x: usize,
    mb_y: usize,
    y: &mut PlaneMut,
    u: &mut PlaneMut,
    v: &mut PlaneMut,
) {
    let limit = i32::from(info.limit);
    if limit == 0 {
        return;
    }
    let il = i32::from(info.ilevel);
    let hev = i32::from(info.hev_thresh);
    let ys = y.stride;
    let yo = y.at(mb_x * 16, mb_y * 16);
    if simple {
        if mb_x > 0 {
            simple_filter(y.data, yo, 1, ys, limit + 4);
        }
        if info.inner {
            for k in 1..4 {
                simple_filter(y.data, yo + 4 * k, 1, ys, limit);
            }
        }
        if mb_y > 0 {
            simple_filter(y.data, yo, ys, 1, limit + 4);
        }
        if info.inner {
            for k in 1..4 {
                simple_filter(y.data, yo + 4 * k * ys, ys, 1, limit);
            }
        }
        return;
    }
    let us = u.stride;
    let uo = u.at(mb_x * 8, mb_y * 8);
    let vo = v.at(mb_x * 8, mb_y * 8);
    if mb_x > 0 {
        loop_filter(y.data, yo, 1, ys, 16, limit + 4, il, hev, true);
        loop_filter(u.data, uo, 1, us, 8, limit + 4, il, hev, true);
        loop_filter(v.data, vo, 1, us, 8, limit + 4, il, hev, true);
    }
    if info.inner {
        for k in 1..4 {
            loop_filter(y.data, yo + 4 * k, 1, ys, 16, limit, il, hev, false);
        }
        loop_filter(u.data, uo + 4, 1, us, 8, limit, il, hev, false);
        loop_filter(v.data, vo + 4, 1, us, 8, limit, il, hev, false);
    }
    if mb_y > 0 {
        loop_filter(y.data, yo, ys, 1, 16, limit + 4, il, hev, true);
        loop_filter(u.data, uo, us, 1, 8, limit + 4, il, hev, true);
        loop_filter(v.data, vo, us, 1, 8, limit + 4, il, hev, true);
    }
    if info.inner {
        for k in 1..4 {
            loop_filter(y.data, yo + 4 * k * ys, ys, 1, 16, limit, il, hev, false);
        }
        loop_filter(u.data, uo + 4 * us, us, 1, 8, limit, il, hev, false);
        loop_filter(v.data, vo + 4 * us, us, 1, 8, limit, il, hev, false);
    }
}
