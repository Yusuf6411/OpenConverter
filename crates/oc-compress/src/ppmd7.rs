//! PPMd variant H (PPMd7) as used by 7z (method 03 04 01) with the 7z range coder. Decoder only.
//!
//! The model lives in one byte arena addressed by 32-bit offsets, exactly like the reference
//! implementation, because the point at which the arena fills up (and the model restarts) is
//! part of the format. Everything in the arena is built by the decoder itself from decoded
//! symbols, so corrupt input can only produce wrong output, never an inconsistent arena.
#![allow(clippy::indexing_slicing)] // arena offsets are produced by the allocator below
use oc_core::{Error, ErrorCode, Limits, Progress, Result};

const MAX_ORDER: u32 = 64;
const MAX_FREQ: u32 = 124;
const UNIT_SIZE: u32 = 12;
const INT_BITS: u32 = 7;
const PERIOD_BITS: u32 = 7;
const BIN_SCALE: u32 = 1 << (INT_BITS + PERIOD_BITS);
const NUM_INDEXES: usize = 38;
const TOP: u32 = 1 << 24;

const INIT_BIN_ESC: [u16; 8] = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];
const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];

#[derive(Clone, Copy, Default)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

#[derive(Clone, Copy)]
enum SeeRef {
    Dummy,
    At(usize, usize),
}

struct Model {
    mem: Vec<u8>,
    size: u32,
    align: u32,
    text: u32,
    units_start: u32,
    lo_unit: u32,
    hi_unit: u32,
    glue_count: u32,
    free_list: [u32; NUM_INDEXES],
    indx2units: [u8; NUM_INDEXES],
    units2indx: [u8; 128],
    ns2indx: [u8; 256],
    ns2bsindx: [u8; 256],
    hb2flag: [u8; 256],
    min_ctx: u32,
    max_ctx: u32,
    found: u32,
    order_fall: u32,
    init_esc: u32,
    prev_success: u32,
    max_order: u32,
    hi_bits_flag: u32,
    run_length: i32,
    init_rl: i32,
    bin_summ: Box<[[u16; 64]; 128]>,
    see: Box<[[See; 16]; 25]>,
    dummy_see: See,
}

// ----- arena accessors -----
impl Model {
    fn u16_at(&self, o: u32) -> u32 {
        let o = o as usize;
        u32::from(u16::from_le_bytes([self.mem[o], self.mem[o + 1]]))
    }
    fn set_u16(&mut self, o: u32, v: u32) {
        let o = o as usize;
        self.mem[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
    }
    fn u32_at(&self, o: u32) -> u32 {
        let o = o as usize;
        u32::from_le_bytes([self.mem[o], self.mem[o + 1], self.mem[o + 2], self.mem[o + 3]])
    }
    fn set_u32(&mut self, o: u32, v: u32) {
        let o = o as usize;
        self.mem[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    // Context: NumStats u16 @0, SummFreq u16 @2, Stats u32 @4, Suffix u32 @8. OneState is @2.
    fn num_stats(&self, c: u32) -> u32 {
        self.u16_at(c)
    }
    fn summ_freq(&self, c: u32) -> u32 {
        self.u16_at(c + 2)
    }
    fn stats(&self, c: u32) -> u32 {
        self.u32_at(c + 4)
    }
    fn suffix(&self, c: u32) -> u32 {
        self.u32_at(c + 8)
    }
    // State (6 bytes): Symbol @0, Freq @1, Successor u32 @2 (low then high half).
    fn sym(&self, s: u32) -> u8 {
        self.mem[s as usize]
    }
    fn freq(&self, s: u32) -> u32 {
        u32::from(self.mem[s as usize + 1])
    }
    fn set_freq(&mut self, s: u32, v: u32) {
        self.mem[s as usize + 1] = v as u8;
    }
    fn successor(&self, s: u32) -> u32 {
        self.u32_at(s + 2)
    }
    fn set_successor(&mut self, s: u32, v: u32) {
        self.set_u32(s + 2, v);
    }
    fn copy_state(&mut self, dst: u32, src: u32) {
        let (d, s) = (dst as usize, src as usize);
        self.mem.copy_within(s..s + 6, d);
    }
    fn read_state(&self, s: u32) -> [u8; 6] {
        let s = s as usize;
        [self.mem[s], self.mem[s + 1], self.mem[s + 2], self.mem[s + 3], self.mem[s + 4], self.mem[s + 5]]
    }
    fn write_state(&mut self, s: u32, v: [u8; 6]) {
        let s = s as usize;
        self.mem[s..s + 6].copy_from_slice(&v);
    }
    fn swap_states(&mut self, a: u32, b: u32) {
        let (x, y) = (self.read_state(a), self.read_state(b));
        self.write_state(a, y);
        self.write_state(b, x);
    }
    fn one_state(c: u32) -> u32 {
        c + 2
    }

    fn i2u(&self, i: usize) -> u32 {
        u32::from(self.indx2units[i])
    }
    fn u2i(&self, nu: u32) -> usize {
        usize::from(self.units2indx[nu as usize - 1])
    }

    fn new(size: u32, order: u32) -> Self {
        let align = 4 - (size & 3);
        let mut m = Model {
            mem: vec![0u8; (align + size + UNIT_SIZE) as usize],
            size,
            align,
            text: 0,
            units_start: 0,
            lo_unit: 0,
            hi_unit: 0,
            glue_count: 0,
            free_list: [0; NUM_INDEXES],
            indx2units: [0; NUM_INDEXES],
            units2indx: [0; 128],
            ns2indx: [0; 256],
            ns2bsindx: [0; 256],
            hb2flag: [0; 256],
            min_ctx: 0,
            max_ctx: 0,
            found: 0,
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order: order,
            hi_bits_flag: 0,
            run_length: 0,
            init_rl: 0,
            bin_summ: Box::new([[0; 64]; 128]),
            see: Box::new([[See::default(); 16]; 25]),
            dummy_see: See { summ: 0, shift: PERIOD_BITS as u8, count: 64 },
        };
        let mut k = 0usize;
        for i in 0..NUM_INDEXES {
            let step = if i >= 12 { 4 } else { (i >> 2) + 1 };
            for _ in 0..step {
                m.units2indx[k] = i as u8;
                k += 1;
            }
            m.indx2units[i] = k as u8;
        }
        m.ns2bsindx[0] = 0;
        m.ns2bsindx[1] = 2;
        for i in 2..11 {
            m.ns2bsindx[i] = 4;
        }
        for i in 11..256 {
            m.ns2bsindx[i] = 6;
        }
        for i in 0..3 {
            m.ns2indx[i] = i as u8;
        }
        let (mut mm, mut kk) = (3u32, 1u32);
        for i in 3..256 {
            m.ns2indx[i] = mm as u8;
            kk -= 1;
            if kk == 0 {
                mm += 1;
                kk = mm - 2;
            }
        }
        for i in 0x40..0x100 {
            m.hb2flag[i] = 8;
        }
        m.restart();
        m
    }

    // ----- allocator -----
    fn insert_node(&mut self, node: u32, indx: usize) {
        let head = self.free_list[indx];
        self.set_u32(node, head);
        self.free_list[indx] = node;
    }

    fn remove_node(&mut self, indx: usize) -> u32 {
        let node = self.free_list[indx];
        self.free_list[indx] = self.u32_at(node);
        node
    }

    fn split_block(&mut self, ptr: u32, old_indx: usize, new_indx: usize) {
        let nu = self.i2u(old_indx) - self.i2u(new_indx);
        let ptr = ptr + self.i2u(new_indx) * UNIT_SIZE;
        let mut i = self.u2i(nu);
        if self.i2u(i) != nu {
            i -= 1;
            let k = self.i2u(i);
            self.insert_node(ptr + k * UNIT_SIZE, (nu - k - 1) as usize);
        }
        self.insert_node(ptr, i);
    }

    // Node: Stamp u16 @0, NU u16 @2, Next u32 @4, Prev u32 @8.
    fn glue_free_blocks(&mut self) {
        let head = self.align + self.size;
        let mut n = head;
        self.glue_count = 255;
        for i in 0..NUM_INDEXES {
            let nu = self.i2u(i);
            let mut next = self.free_list[i];
            self.free_list[i] = 0;
            while next != 0 {
                let node = next;
                self.set_u32(node + 4, n);
                self.set_u32(n + 8, next);
                n = next;
                next = self.u32_at(node);
                self.set_u16(node, 0);
                self.set_u16(node + 2, nu);
            }
        }
        self.set_u16(head, 1);
        self.set_u32(head + 4, n);
        self.set_u32(n + 8, head);
        if self.lo_unit != self.hi_unit {
            self.set_u16(self.lo_unit, 1);
        }
        // Glue adjacent free blocks.
        while n != head {
            let node = n;
            let mut nu = self.u16_at(node + 2);
            loop {
                let node2 = node + nu * UNIT_SIZE;
                nu += self.u16_at(node2 + 2);
                if self.u16_at(node2) != 0 || nu >= 0x10000 {
                    break;
                }
                let (prev2, next2) = (self.u32_at(node2 + 8), self.u32_at(node2 + 4));
                self.set_u32(prev2 + 4, next2);
                self.set_u32(next2 + 8, prev2);
                self.set_u16(node + 2, nu);
            }
            n = self.u32_at(node + 4);
        }
        // Refill the free lists.
        n = self.u32_at(head + 4);
        while n != head {
            let mut node = n;
            let next = self.u32_at(node + 4);
            let mut nu = self.u16_at(node + 2);
            while nu > 128 {
                self.insert_node(node, NUM_INDEXES - 1);
                nu -= 128;
                node += 128 * UNIT_SIZE;
            }
            let mut i = self.u2i(nu);
            if self.i2u(i) != nu {
                i -= 1;
                let k = self.i2u(i);
                self.insert_node(node + k * UNIT_SIZE, (nu - k - 1) as usize);
            }
            self.insert_node(node, i);
            n = next;
        }
    }

    /// Returns 0 when the arena is exhausted.
    fn alloc_units_rare(&mut self, indx: usize) -> u32 {
        if self.glue_count == 0 {
            self.glue_free_blocks();
            if self.free_list[indx] != 0 {
                return self.remove_node(indx);
            }
        }
        let mut i = indx;
        loop {
            i += 1;
            if i == NUM_INDEXES {
                let num_bytes = self.i2u(indx) * UNIT_SIZE;
                self.glue_count -= 1;
                return if self.units_start - self.text > num_bytes {
                    self.units_start -= num_bytes;
                    self.units_start
                } else {
                    0
                };
            }
            if self.free_list[i] != 0 {
                break;
            }
        }
        let r = self.remove_node(i);
        self.split_block(r, i, indx);
        r
    }

    fn alloc_units(&mut self, indx: usize) -> u32 {
        if self.free_list[indx] != 0 {
            return self.remove_node(indx);
        }
        let num_bytes = self.i2u(indx) * UNIT_SIZE;
        if num_bytes <= self.hi_unit - self.lo_unit {
            let r = self.lo_unit;
            self.lo_unit += num_bytes;
            return r;
        }
        self.alloc_units_rare(indx)
    }

    fn copy_units(&mut self, dst: u32, src: u32, nu: u32) {
        let (d, s, n) = (dst as usize, src as usize, (nu * UNIT_SIZE) as usize);
        self.mem.copy_within(s..s + n, d);
    }

    fn shrink_units(&mut self, old: u32, old_nu: u32, new_nu: u32) -> u32 {
        let (i0, i1) = (self.u2i(old_nu), self.u2i(new_nu));
        if i0 == i1 {
            return old;
        }
        if self.free_list[i1] != 0 {
            let ptr = self.remove_node(i1);
            self.copy_units(ptr, old, new_nu);
            self.insert_node(old, i0);
            return ptr;
        }
        self.split_block(old, i0, i1);
        old
    }

    // ----- model -----
    fn restart(&mut self) {
        self.free_list = [0; NUM_INDEXES];
        self.text = self.align;
        self.hi_unit = self.text + self.size;
        self.units_start = self.hi_unit - self.size / 8 / UNIT_SIZE * 7 * UNIT_SIZE;
        self.lo_unit = self.units_start;
        self.glue_count = 0;
        self.order_fall = self.max_order;
        self.init_rl = -(self.max_order.min(12) as i32) - 1;
        self.run_length = self.init_rl;
        self.prev_success = 0;

        self.hi_unit -= UNIT_SIZE;
        let c = self.hi_unit;
        self.min_ctx = c;
        self.max_ctx = c;
        self.set_u32(c + 8, 0);
        self.set_u16(c, 256);
        self.set_u16(c + 2, 257);
        self.found = self.lo_unit;
        self.set_u32(c + 4, self.lo_unit);
        self.lo_unit += (256 / 2) * UNIT_SIZE;
        for i in 0..256u32 {
            let s = self.found + i * 6;
            self.write_state(s, [i as u8, 1, 0, 0, 0, 0]);
        }
        for i in 0..128usize {
            for k in 0..8usize {
                let val = (BIN_SCALE - u32::from(INIT_BIN_ESC[k]) / (i as u32 + 2)) as u16;
                for m in (0..64).step_by(8) {
                    self.bin_summ[i][k + m] = val;
                }
            }
        }
        for i in 0..25usize {
            for k in 0..16usize {
                self.see[i][k] = See { summ: ((5 * i as u32 + 10) << (PERIOD_BITS - 4)) as u16, shift: (PERIOD_BITS - 4) as u8, count: 4 };
            }
        }
    }

    /// Returns 0 when the arena is exhausted.
    fn create_successors(&mut self, skip: bool) -> u32 {
        let mut c = self.min_ctx;
        let up_branch = self.successor(self.found);
        let found_sym = self.sym(self.found);
        let mut ps = [0u32; MAX_ORDER as usize];
        let mut num_ps = 0usize;
        if !skip {
            ps[num_ps] = self.found;
            num_ps += 1;
        }
        while self.suffix(c) != 0 {
            c = self.suffix(c);
            let s = if self.num_stats(c) != 1 {
                let mut s = self.stats(c);
                while self.sym(s) != found_sym {
                    s += 6;
                }
                s
            } else {
                Self::one_state(c)
            };
            let successor = self.successor(s);
            if successor != up_branch {
                c = successor;
                if num_ps == 0 {
                    return c;
                }
                break;
            }
            ps[num_ps] = s;
            num_ps += 1;
        }
        let up_sym = self.mem[up_branch as usize];
        let up_succ = up_branch + 1;
        let up_freq = if self.num_stats(c) == 1 {
            self.freq(Self::one_state(c))
        } else {
            let mut s = self.stats(c);
            while self.sym(s) != up_sym {
                s += 6;
            }
            let cf = self.freq(s) - 1;
            let s0 = self.summ_freq(c) - self.num_stats(c) - cf;
            1 + if 2 * cf <= s0 { u32::from(5 * cf > s0) } else { (2 * cf + 3 * s0 - 1) / (2 * s0) }
        };
        loop {
            let c1 = if self.hi_unit != self.lo_unit {
                self.hi_unit -= UNIT_SIZE;
                self.hi_unit
            } else if self.free_list[0] != 0 {
                self.remove_node(0)
            } else {
                let r = self.alloc_units_rare(0);
                if r == 0 {
                    return 0;
                }
                r
            };
            self.set_u16(c1, 1);
            let one = Self::one_state(c1);
            let sb = up_succ.to_le_bytes();
            self.write_state(one, [up_sym, up_freq as u8, sb[0], sb[1], sb[2], sb[3]]);
            self.set_u32(c1 + 8, c);
            num_ps -= 1;
            self.set_successor(ps[num_ps], c1);
            c = c1;
            if num_ps == 0 {
                break;
            }
        }
        c
    }

    fn update_model(&mut self) {
        let found = self.found;
        let found_sym = self.sym(found);
        let found_freq = self.freq(found);
        let mut f_successor = self.successor(found);

        if found_freq < MAX_FREQ / 4 && self.suffix(self.min_ctx) != 0 {
            let c = self.suffix(self.min_ctx);
            if self.num_stats(c) == 1 {
                let s = Self::one_state(c);
                if self.freq(s) < 32 {
                    self.set_freq(s, self.freq(s) + 1);
                }
            } else {
                let mut s = self.stats(c);
                if self.sym(s) != found_sym {
                    loop {
                        s += 6;
                        if self.sym(s) == found_sym {
                            break;
                        }
                    }
                    if self.freq(s) >= self.freq(s - 6) {
                        self.swap_states(s, s - 6);
                        s -= 6;
                    }
                }
                if self.freq(s) < MAX_FREQ - 9 {
                    self.set_freq(s, self.freq(s) + 2);
                    self.set_u16(c + 2, self.summ_freq(c) + 2);
                }
            }
        }

        if self.order_fall == 0 {
            let c = self.create_successors(true);
            self.min_ctx = c;
            self.max_ctx = c;
            if c == 0 {
                self.restart();
                return;
            }
            self.set_successor(self.found, c);
            return;
        }

        self.mem[self.text as usize] = found_sym;
        self.text += 1;
        let mut successor = self.text;
        if self.text >= self.units_start {
            self.restart();
            return;
        }

        if f_successor != 0 {
            if f_successor <= successor {
                let cs = self.create_successors(false);
                if cs == 0 {
                    self.restart();
                    return;
                }
                f_successor = cs;
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                successor = f_successor;
                if self.max_ctx != self.min_ctx {
                    self.text -= 1;
                }
            }
        } else {
            self.set_successor(self.found, successor);
            f_successor = self.min_ctx;
        }

        let ns = self.num_stats(self.min_ctx);
        let found_freq = self.freq(self.found);
        let s0 = self.summ_freq(self.min_ctx) - ns - (found_freq - 1);

        let mut c = self.max_ctx;
        while c != self.min_ctx {
            let ns1 = self.num_stats(c);
            if ns1 != 1 {
                if ns1 & 1 == 0 {
                    let old_nu = ns1 >> 1;
                    let i = self.u2i(old_nu);
                    if i != self.u2i(old_nu + 1) {
                        let ptr = self.alloc_units(i + 1);
                        if ptr == 0 {
                            self.restart();
                            return;
                        }
                        let old = self.stats(c);
                        self.copy_units(ptr, old, old_nu);
                        self.insert_node(old, i);
                        self.set_u32(c + 4, ptr);
                    }
                }
                let sf = self.summ_freq(c);
                let add = u32::from(2 * ns1 < ns) + 2 * u32::from((4 * ns1 <= ns) & (sf <= 8 * ns1));
                self.set_u16(c + 2, sf + add);
            } else {
                let s = self.alloc_units(0);
                if s == 0 {
                    self.restart();
                    return;
                }
                self.copy_state(s, Self::one_state(c));
                self.set_u32(c + 4, s);
                let f = self.freq(s);
                let nf = if f < MAX_FREQ / 4 - 1 { f << 1 } else { MAX_FREQ - 4 };
                self.set_freq(s, nf);
                self.set_u16(c + 2, nf + self.init_esc + u32::from(ns > 3));
            }
            let sfc = self.summ_freq(c);
            let mut cf = 2 * found_freq * (sfc + 6);
            let sf = s0 + sfc;
            if cf < 6 * sf {
                cf = 1 + u32::from(cf > sf) + u32::from(cf >= 4 * sf);
                self.set_u16(c + 2, sfc + 3);
            } else {
                cf = 4 + u32::from(cf >= 9 * sf) + u32::from(cf >= 12 * sf) + u32::from(cf >= 15 * sf);
                self.set_u16(c + 2, sfc + cf);
            }
            let s = self.stats(c) + ns1 * 6;
            let sb = successor.to_le_bytes();
            self.write_state(s, [found_sym, cf as u8, sb[0], sb[1], sb[2], sb[3]]);
            self.set_u16(c, ns1 + 1);
            c = self.suffix(c);
        }
        self.max_ctx = f_successor;
        self.min_ctx = f_successor;
    }

    fn rescale(&mut self) {
        let mc = self.min_ctx;
        let stats = self.stats(mc);
        let mut s = self.found;
        if s != stats {
            let tmp = self.read_state(s);
            while s != stats {
                self.copy_state(s, s - 6);
                s -= 6;
            }
            self.write_state(s, tmp);
        }
        let mut esc_freq = self.summ_freq(mc) - self.freq(s);
        let adder = u32::from(self.order_fall != 0);
        let f = (self.freq(s) + 4 + adder) >> 1;
        self.set_freq(s, f);
        let mut sum_freq = f;
        let mut i = self.num_stats(mc) - 1;
        while i > 0 {
            s += 6;
            esc_freq -= self.freq(s);
            let f = (self.freq(s) + adder) >> 1;
            self.set_freq(s, f);
            sum_freq += f;
            if f > self.freq(s - 6) {
                let tmp = self.read_state(s);
                let mut s1 = s;
                loop {
                    self.copy_state(s1, s1 - 6);
                    s1 -= 6;
                    if s1 == stats || u32::from(tmp[1]) <= self.freq(s1 - 6) {
                        break;
                    }
                }
                self.write_state(s1, tmp);
            }
            i -= 1;
        }
        if self.freq(s) == 0 {
            let num_stats = self.num_stats(mc);
            let mut removed = 0u32;
            loop {
                removed += 1;
                s -= 6;
                if self.freq(s) != 0 {
                    break;
                }
            }
            esc_freq += removed;
            let new_ns = num_stats - removed;
            self.set_u16(mc, new_ns);
            if new_ns == 1 {
                let mut tmp = self.read_state(stats);
                loop {
                    tmp[1] -= tmp[1] >> 1;
                    esc_freq >>= 1;
                    if esc_freq <= 1 {
                        break;
                    }
                }
                let idx = self.u2i((num_stats + 1) >> 1);
                self.insert_node(stats, idx);
                self.found = Self::one_state(mc);
                self.write_state(self.found, tmp);
                return;
            }
            let n0 = (num_stats + 1) >> 1;
            let n1 = (new_ns + 1) >> 1;
            if n0 != n1 {
                let ns = self.shrink_units(stats, n0, n1);
                self.set_u32(mc + 4, ns);
            }
        }
        self.set_u16(mc + 2, sum_freq + esc_freq - (esc_freq >> 1));
        self.found = self.stats(mc);
    }

    fn make_esc_freq(&mut self, num_masked: u32) -> (SeeRef, u32) {
        let mc = self.min_ctx;
        let ns = self.num_stats(mc);
        let non_masked = ns - num_masked;
        if ns != 256 {
            let i = usize::from(self.ns2indx[non_masked as usize - 1]);
            let suffix_ns = self.num_stats(self.suffix(mc));
            let k = u32::from(non_masked < suffix_ns.wrapping_sub(ns))
                + 2 * u32::from(self.summ_freq(mc) < 11 * ns)
                + 4 * u32::from(num_masked > non_masked)
                + self.hi_bits_flag;
            let see = &mut self.see[i][k as usize];
            let r = u32::from(see.summ >> see.shift);
            see.summ = see.summ.wrapping_sub(r as u16);
            (SeeRef::At(i, k as usize), r + u32::from(r == 0))
        } else {
            (SeeRef::Dummy, 1)
        }
    }

    fn see_mut(&mut self, r: SeeRef) -> &mut See {
        match r {
            SeeRef::Dummy => &mut self.dummy_see,
            SeeRef::At(i, k) => &mut self.see[i][k],
        }
    }

    fn see_update(&mut self, r: SeeRef) {
        let see = self.see_mut(r);
        if u32::from(see.shift) < PERIOD_BITS {
            see.count = see.count.wrapping_sub(1);
            if see.count == 0 {
                see.summ = see.summ.wrapping_shl(1);
                see.count = (3u32 << see.shift) as u8;
                see.shift += 1;
            }
        }
    }

    fn next_context(&mut self) {
        let c = self.successor(self.found);
        if self.order_fall == 0 && c > self.text {
            self.min_ctx = c;
            self.max_ctx = c;
        } else {
            self.update_model();
        }
    }

    fn update1(&mut self) {
        let mut s = self.found;
        self.set_freq(s, self.freq(s) + 4);
        self.set_u16(self.min_ctx + 2, self.summ_freq(self.min_ctx) + 4);
        if self.freq(s) > self.freq(s - 6) {
            self.swap_states(s, s - 6);
            s -= 6;
            self.found = s;
            if self.freq(s) > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context();
    }

    fn update1_0(&mut self) {
        let f = self.freq(self.found);
        self.prev_success = u32::from(2 * f > self.summ_freq(self.min_ctx));
        self.run_length += self.prev_success as i32;
        self.set_u16(self.min_ctx + 2, self.summ_freq(self.min_ctx) + 4);
        self.set_freq(self.found, f + 4);
        if f + 4 > MAX_FREQ {
            self.rescale();
        }
        self.next_context();
    }

    fn update_bin(&mut self) {
        let f = self.freq(self.found);
        self.set_freq(self.found, f + u32::from(f < 128));
        self.prev_success = 1;
        self.run_length += 1;
        self.next_context();
    }

    fn update2(&mut self) {
        self.set_u16(self.min_ctx + 2, self.summ_freq(self.min_ctx) + 4);
        let f = self.freq(self.found) + 4;
        self.set_freq(self.found, f);
        if f > MAX_FREQ {
            self.rescale();
        }
        self.run_length = self.init_rl;
        self.update_model();
    }
}

struct RangeDec<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl RangeDec<'_> {
    fn byte(&mut self) -> u32 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        u32::from(b)
    }
    fn normalize(&mut self) {
        if self.range < TOP {
            self.code = (self.code << 8) | self.byte();
            self.range <<= 8;
            if self.range < TOP {
                self.code = (self.code << 8) | self.byte();
                self.range <<= 8;
            }
        }
    }
    fn threshold(&mut self, total: u32) -> u32 {
        self.range /= total;
        self.code / self.range
    }
    fn decode(&mut self, start: u32, size: u32) {
        self.code = self.code.wrapping_sub(start.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(size);
        self.normalize();
    }
    fn bit(&mut self, size0: u32, total: u32) -> u32 {
        let bound = (self.range / total) * size0;
        let sym = if self.code < bound {
            self.range = bound;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            1
        };
        self.normalize();
        sym
    }
}

fn corrupt() -> Error {
    Error::corrupt("PPMd: data error")
}

impl Model {
    /// One symbol; `None` at the end mark or on a data error.
    fn decode_symbol(&mut self, rc: &mut RangeDec<'_>) -> Result<u8> {
        let mut mask = [0xFFu8; 256];
        let mc = self.min_ctx;
        if self.num_stats(mc) != 1 {
            let mut s = self.stats(mc);
            let summ = self.summ_freq(mc);
            let count = rc.threshold(summ);
            let mut hi = self.freq(s);
            if count < hi {
                rc.decode(0, hi);
                self.found = s;
                let sym = self.sym(s);
                self.update1_0();
                return Ok(sym);
            }
            self.prev_success = 0;
            let mut i = self.num_stats(mc) - 1;
            loop {
                s += 6;
                hi += self.freq(s);
                if hi > count {
                    rc.decode(hi - self.freq(s), self.freq(s));
                    self.found = s;
                    let sym = self.sym(s);
                    self.update1();
                    return Ok(sym);
                }
                i -= 1;
                if i == 0 {
                    break;
                }
            }
            if count >= summ {
                return Err(corrupt());
            }
            self.hi_bits_flag = u32::from(self.hb2flag[usize::from(self.sym(self.found))]);
            rc.decode(hi, summ - hi);
            mask[usize::from(self.sym(s))] = 0;
            let mut i = self.num_stats(mc) - 1;
            while i > 0 {
                s -= 6;
                mask[usize::from(self.sym(s))] = 0;
                i -= 1;
            }
        } else {
            let one = Self::one_state(mc);
            self.hi_bits_flag = u32::from(self.hb2flag[usize::from(self.sym(self.found))]);
            let idx0 = self.freq(one) as usize - 1;
            let idx1 = self.prev_success
                + u32::from(self.ns2bsindx[self.num_stats(self.suffix(mc)) as usize - 1])
                + self.hi_bits_flag
                + 2 * u32::from(self.hb2flag[usize::from(self.sym(one))])
                + (((self.run_length >> 26) & 0x20) as u32);
            let prob = u32::from(self.bin_summ[idx0][idx1 as usize]);
            let mean = (prob + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS;
            if rc.bit(prob, BIN_SCALE) == 0 {
                self.bin_summ[idx0][idx1 as usize] = (prob + (1 << INT_BITS) - mean) as u16;
                self.found = one;
                let sym = self.sym(one);
                self.update_bin();
                return Ok(sym);
            }
            let np = prob - mean;
            self.bin_summ[idx0][idx1 as usize] = np as u16;
            self.init_esc = u32::from(EXP_ESCAPE[(np >> 10) as usize]);
            mask[usize::from(self.sym(one))] = 0;
            self.prev_success = 0;
        }
        let mut ps = [0u32; 256];
        loop {
            let num_masked = self.num_stats(self.min_ctx);
            loop {
                self.order_fall += 1;
                let suffix = self.suffix(self.min_ctx);
                if suffix == 0 {
                    return Err(Error::new(ErrorCode::CorruptFile, "PPMd: end mark inside the data"));
                }
                self.min_ctx = suffix;
                if self.num_stats(suffix) != num_masked {
                    break;
                }
            }
            let mc = self.min_ctx;
            let mut hi = 0u32;
            let mut s = self.stats(mc);
            let num = (self.num_stats(mc) - num_masked) as usize;
            let mut i = 0usize;
            while i != num {
                if mask[usize::from(self.sym(s))] != 0 {
                    hi += self.freq(s);
                    ps[i] = s;
                    i += 1;
                }
                s += 6;
            }
            let (see, esc) = self.make_esc_freq(num_masked);
            let freq_sum = esc + hi;
            let count = rc.threshold(freq_sum);
            if count < hi {
                let mut k = 0usize;
                let mut acc = self.freq(ps[0]);
                while acc <= count {
                    k += 1;
                    acc += self.freq(ps[k]);
                }
                let s = ps[k];
                rc.decode(acc - self.freq(s), self.freq(s));
                self.see_update(see);
                self.found = s;
                let sym = self.sym(s);
                self.update2();
                return Ok(sym);
            }
            if count >= freq_sum {
                return Err(corrupt());
            }
            rc.decode(hi, freq_sum - hi);
            let sm = self.see_mut(see);
            sm.summ = sm.summ.wrapping_add(freq_sum as u16);
            for &p in &ps[..num] {
                mask[usize::from(self.sym(p))] = 0;
            }
        }
    }
}

/// Decodes `out_len` bytes of a 7z PPMd stream. `props` is the order (1 byte) followed by the
/// model memory size (u32, little endian).
pub fn decode(props: &[u8], data: &[u8], out_len: usize, limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let [order, m0, m1, m2, m3, ..] = *props else {
        return Err(Error::corrupt("PPMd: properties are 5 bytes"));
    };
    let mem = u32::from_le_bytes([m0, m1, m2, m3]);
    let order = u32::from(order);
    if !(2..=MAX_ORDER).contains(&order) || mem < (1 << 11) || mem > 0xFFFF_FFFF - 12 * 3 {
        return Err(Error::new(ErrorCode::UnsupportedVariant, "PPMd: unsupported order or memory size"));
    }
    limits.check_alloc(u64::from(mem))?;
    limits.check_alloc(out_len as u64)?;
    if data.len() < 5 || data[0] != 0 {
        return Err(Error::corrupt("PPMd: bad range coder header"));
    }
    let mut rc = RangeDec {
        data,
        pos: 5,
        range: 0xFFFF_FFFF,
        code: u32::from_be_bytes([data[1], data[2], data[3], data[4]]),
    };
    if rc.code == 0xFFFF_FFFF {
        return Err(Error::corrupt("PPMd: bad range coder header"));
    }
    let mut model = Model::new(mem, order);
    let mut out = Vec::with_capacity(out_len.min(1 << 26));
    while out.len() < out_len {
        if out.len() & 0xFFFF == 0 {
            progress.check_cancelled()?;
        }
        out.push(model.decode_symbol(&mut rc)?);
        if rc.pos > data.len() + 4 {
            return Err(Error::truncated("PPMd: input ends early"));
        }
    }
    Ok(out)
}
