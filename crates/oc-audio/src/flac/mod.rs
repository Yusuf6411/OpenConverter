//! FLAC (RFC 9639): all block sizes, constant / verbatim / fixed / LPC subframes, the four
//! stereo modes, 4 to 32 bit samples, Rice partitions, Vorbis comments, pictures, seek tables
//! and the MD5 of the audio.
mod dec;
mod enc;

pub use dec::{decode_stream, parse_streaminfo, read, sniff, split_frames, StreamInfo};
pub use enc::{write, Options};

/// CRC-8 (polynomial 0x07) over a frame header.
pub(crate) fn crc8(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
    }
    c
}

static CRC16: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-16 (polynomial 0x8005, MSB first, initial value 0) over a whole frame.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in data {
        c = (c << 8) ^ CRC16[usize::from((c >> 8) as u8 ^ b)];
    }
    c
}
