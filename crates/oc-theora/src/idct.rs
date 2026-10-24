//! The exact integer inverse DCT of spec 7.9.3. Every intermediate follows the order and the
//! 16-bit truncations of the specification, since any drift would accumulate through the
//! prediction loop.

const C1: i32 = 64277;
const C2: i32 = 60547;
const C3: i32 = 54491;
const C4: i32 = 46341;
const C5: i32 = 36410;
const C6: i32 = 25080;
const C7: i32 = 12785;
const S1: i32 = C7;
const S2: i32 = C6;
const S3: i32 = C5;
const S6: i32 = C2;
const S7: i32 = C1;

#[inline]
fn t16(x: i32) -> i32 {
    i32::from(x as i16)
}

/// The 1D inverse DCT of eight 16-bit values.
fn idct8(y: [i32; 8]) -> [i32; 8] {
    let mut t = [0i32; 8];
    t[0] = (C4 * t16(y[0] + y[4])) >> 16;
    t[1] = (C4 * t16(y[0] - y[4])) >> 16;
    t[2] = ((C6 * y[2]) >> 16) - ((S6 * y[6]) >> 16);
    t[3] = ((S6 * y[2]) >> 16) + ((C6 * y[6]) >> 16);
    t[4] = ((C7 * y[1]) >> 16) - ((S7 * y[7]) >> 16);
    t[5] = ((C3 * y[5]) >> 16) - ((S3 * y[3]) >> 16);
    t[6] = ((S3 * y[5]) >> 16) + ((C3 * y[3]) >> 16);
    t[7] = ((S7 * y[1]) >> 16) + ((C7 * y[7]) >> 16);
    let r = t[4] + t[5];
    t[5] = (C4 * t16(t[4] - t[5])) >> 16;
    t[4] = r;
    let r = t[7] + t[6];
    t[6] = (C4 * t16(t[7] - t[6])) >> 16;
    t[7] = r;
    let r = t[0] + t[3];
    t[3] = t[0] - t[3];
    t[0] = r;
    let r = t[1] + t[2];
    t[2] = t[1] - t[2];
    t[1] = r;
    let r = t[6] + t[5];
    t[5] = t[6] - t[5];
    t[6] = r;
    [
        t16(t[0] + t[7]),
        t16(t[1] + t[6]),
        t16(t[2] + t[5]),
        t16(t[3] + t[4]),
        t16(t[3] - t[4]),
        t16(t[2] - t[5]),
        t16(t[1] - t[6]),
        t16(t[0] - t[7]),
    ]
}

/// The 2D inverse DCT: rows first, then columns, then a rounded division by sixteen.
pub fn idct2d(dqc: &[i32; 64], res: &mut [i32; 64]) {
    let mut tmp = [0i32; 64];
    for r in 0..8 {
        let mut y = [0i32; 8];
        y.copy_from_slice(&dqc[r * 8..r * 8 + 8]);
        let x = idct8(y);
        tmp[r * 8..r * 8 + 8].copy_from_slice(&x);
    }
    for c in 0..8 {
        let mut y = [0i32; 8];
        for r in 0..8 {
            y[r] = tmp[r * 8 + c];
        }
        let x = idct8(y);
        for r in 0..8 {
            res[r * 8 + c] = (x[r] + 8) >> 4;
        }
    }
}

#[allow(dead_code)]
const _UNUSED: [i32; 3] = [C5 + S1, S2, C1 + C2];
