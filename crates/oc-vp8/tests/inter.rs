//! Inter-frame decoding against a stream made by libvpx; the expected hash is that of the
//! pictures ffmpeg's VP8 decoder produces for the same frames.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use oc_core::NoProgress;

#[test]
fn decodes_a_libvpx_stream_bit_exactly() {
    let data = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/inter_80x64.bin"
    ))
    .unwrap();
    let mut dec = oc_vp8::Decoder::new();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let (mut pos, mut shown, mut inter) = (0, 0, 0);
    while pos < data.len() {
        let n = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let frame = &data[pos + 4..pos + 4 + n];
        pos += 4 + n;
        inter += usize::from(frame[0] & 1);
        if let Some(f) = dec.decode(frame, &NoProgress).unwrap() {
            shown += 1;
            for b in f.y.iter().chain(&f.u).chain(&f.v) {
                hash = (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    assert_eq!((shown, inter), (16, 14));
    assert_eq!(hash, 0xf783_a353_1f88_59e7);
}

#[test]
fn an_inter_frame_before_a_key_frame_is_an_error() {
    let mut dec = oc_vp8::Decoder::new();
    assert!(dec
        .decode(&[0x01, 0x00, 0x00, 0, 0, 0], &NoProgress)
        .is_err());
}
