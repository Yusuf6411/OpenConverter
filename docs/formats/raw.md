# Camera raw: DNG

Adobe DNG 1.7 is read (not written) and developed to a 16-bit sRGB picture, so it converts to any image format. The code is `crates/oc-image/src/dng.rs`; the specification is kept in [../specs/dng_spec_1_7_1_0.pdf](../specs/dng_spec_1_7_1_0.pdf).

## What is read
- The full-resolution raw directory, found in IFD 0 or a SubIFD, as a colour filter array (any repeat pattern of red, green and blue) or as LinearRaw (three samples per pixel).
- Strips or tiles; uncompressed (8 to 16 bits, packed MSB first), lossless JPEG (through the lossless mode of our JPEG decoder) or Deflate with the horizontal predictor.
- LinearizationTable, BlackLevel with its repeat pattern and the per-column and per-row deltas, WhiteLevel, ActiveArea, DefaultCropOrigin and DefaultCropSize, Orientation (applied), BaselineExposure.
- Colour: ForwardMatrix when present, otherwise ColorMatrix with Bradford adaptation; two calibration illuminants are blended by inverse colour temperature as chapter 6 of the specification requires, and the white point comes from AsShotNeutral or AsShotWhiteXY by the specified iteration.

## How it was checked
`tools/make_dng_fixtures.py` writes synthetic DNG files from a scene with known colours: the scene goes through a made-up camera matrix, is mosaicked, quantised and stored in each layout. The tests (`crates/oc-image/tests/dng.rs`) decode them and compare with the scene: the mean error is about 0.2 of an 8-bit level and the largest 1, for 16-bit strips, 12-bit packed big-endian data in a different filter order, 14-bit lossless-JPEG tiles, Deflate with two illuminants and forward matrices, a 90-degree orientation, and an active area with a default crop. The scene is linear in x and y so bilinear demosaicing is exact in it; this checks the plumbing and the colour arithmetic, not how a real camera looks.

## Not done
- No real camera file was available, so nothing here has met a real sensor. Cameras whose files rely on opcode lists (gain maps, lens corrections), lossy JPEG (34892) or JPEG XL raw data, more than three colour planes (RGBE, CMYG) or floating-point samples are refused with a typed error or rendered without those steps.
- The render is plain: bilinear demosaicing (a nearest-same-colour mean, which also copes with X-Trans patterns roughly), no camera tone curve, hue/saturation map, noise reduction or highlight recovery. The master prompt names AHD demosaicing; it is not written.
- Proprietary raw formats (CR2, CR3, NEF, ARW and so on) have no public specification and are not read.
