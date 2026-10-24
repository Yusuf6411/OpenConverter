# Ogg Vorbis

Own Ogg page reader/writer (`ogg.rs`) and Vorbis I codec (`oc-audio::vorbis`).

- **Decode**: floor type 1, residue types 0 to 2, square-polar channel coupling, 1 to 8 channels in Vorbis's channel orders (reordered to WAVE order), granule-position trimming, chained streams (first link only, noted). Matches ffmpeg to 5e-8 on fixtures.
- **Encode**: floor 1, residue 1, coupling, quality -1 to 10 with the libvorbis bit-rate ladder, one block size (2048) throughout. Size and SNR are within a few percent of libvorbis at qualities 0 to 6; ffmpeg decodes the output without warnings.

Not done: floor type 0, short blocks in the encoder (so sharp attacks pre-echo more than in libvorbis), bit-rate managed (ABR) encoding.
