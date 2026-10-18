import random
import unittest
import zlib  # tests only: interop cross-checks

from openconverter.codecs import deflate as D
from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant

BIG = 1 << 30


def corpus():
    rng = random.Random(2024)
    words = [b"alpha ", b"beta ", b"gamma ", b"delta\n", b"epsilon, ", b"zeta"]
    rnd = bytes(rng.getrandbits(8) for _ in range(40_000))
    return {
        "empty": b"",
        "one": b"x",
        "two": b"xy",
        "zeros": bytes(50_000),
        "repetitive": b"abcabcabd" * 6_000,
        "random": rnd,
        "text": b"".join(rng.choice(words) for _ in range(8_000)),
        "beyond_window": rnd[:33_000] * 2,   # repeat 33000 bytes back: out of reach
        "edge_window": rnd[:32_000] * 2,     # repeat 32000 bytes back: within the window
        "ramp": bytes(i & 255 for i in range(70_000)),
    }


def bits_to_bytes(fields):
    """[(value, nbits), ...] -> LSB-first packed bytes."""
    bw = D._BitWriter()
    for v, n in fields:
        bw.write(v, n)
    return bw.getvalue()


def fixed_code(sym):
    code, length = D.canonical_codes(D.FIXED_LITLEN_LENGTHS)[sym]
    return D._reverse(code, length), length


def dynamic_header(lit_lengths, dist_lengths, cl_override=None):
    """Fields for a dynamic block header (BFINAL=1) with the given code lengths."""
    rle = D._rle_code_lengths(lit_lengths + dist_lengths)
    cl_f = [0] * 19
    for sym, _, _ in rle:
        cl_f[sym] += 1
    cl_len = cl_override or D.huffman_lengths(cl_f, 7)
    fields = [(1, 1), (2, 2), (len(lit_lengths) - 257, 5), (len(dist_lengths) - 1, 5), (15, 4)]
    fields += [(cl_len[D.CODELEN_ORDER[i]], 3) for i in range(19)]
    enc = D._encode_table(cl_len)
    for sym, xb, xv in rle:
        fields.append(enc[sym])
        if xb:
            fields.append((xv, xb))
    return fields


class InteropTests(unittest.TestCase):
    def test_ours_decodes_with_zlib_all_levels(self):
        for name, data in corpus().items():
            levels = range(10) if len(data) < 60_000 else (0, 1, 4, 6, 9)
            for level in levels:
                z = D.zlib_compress(data, level)
                self.assertEqual(zlib.decompress(z), data, f"{name} level {level}")
                self.assertEqual(D.zlib_decompress(z, BIG), data, f"{name} level {level}")

    def test_zlib_streams_decode_with_ours(self):
        for name, data in corpus().items():
            for level in (0, 1, 6, 9):
                self.assertEqual(D.zlib_decompress(zlib.compress(data, level), BIG), data,
                                 f"{name} level {level}")
            raw = zlib.compressobj(9, zlib.DEFLATED, -15, 9, zlib.Z_HUFFMAN_ONLY)
            stream = raw.compress(data) + raw.flush()
            self.assertEqual(D.inflate(stream, BIG), data, name)

    def test_compresses(self):
        data = corpus()["text"]
        self.assertLess(len(D.deflate(data, 6)), len(data) // 3)
        self.assertLessEqual(len(D.deflate(data, 9)), len(D.deflate(data, 1)))


class BlockTypeTests(unittest.TestCase):
    def test_stored_blocks_level0_and_incompressible(self):
        self.assertEqual(set(D.block_types(D.deflate(b"hello world" * 10, 0))), {0})
        rnd = corpus()["random"]
        self.assertIn(0, D.block_types(D.deflate(rnd, 6)))
        big = bytes(70_000)
        self.assertEqual(D.block_types(D.deflate(big, 0)), [0, 0])  # > 65535 bytes splits

    def test_fixed_block_for_short_input(self):
        self.assertEqual(D.block_types(D.deflate(b"abcabcabcabc", 6)), [1])

    def test_dynamic_block_for_text(self):
        self.assertIn(2, D.block_types(D.deflate(corpus()["text"], 6)))

    def test_multiple_blocks(self):
        rng = random.Random(9)
        data = bytes(rng.getrandbits(8) for _ in range(100_000))
        stream = D.deflate(data, 1)
        self.assertGreater(len(D.block_types(stream)), 2)
        self.assertEqual(zlib.decompress(stream, -15), data)

    def test_zlib_fixed_and_stored_inputs(self):
        comp = zlib.compressobj(6, zlib.DEFLATED, -15, 9, zlib.Z_FIXED)
        stream = comp.compress(b"fixed huffman please" * 50) + comp.flush()
        self.assertEqual(D.block_types(stream), [1])
        self.assertEqual(D.inflate(stream, BIG), b"fixed huffman please" * 50)


class MalformedTests(unittest.TestCase):
    def assertCorrupt(self, raw):
        with self.assertRaises(CorruptFile):
            D.inflate(raw, BIG)

    def test_block_type_3(self):
        self.assertCorrupt(bytes([0x07, 0x00]))

    def test_stored_len_nlen_mismatch(self):
        self.assertCorrupt(bytes([0x01, 0x05, 0x00, 0x00, 0x00]) + b"hello")

    def test_truncated(self):
        raw = zlib.compress(b"hello world " * 500)[2:-4]
        for cut in (1, 2, 5, len(raw) // 2, len(raw) - 1):
            self.assertCorrupt(raw[:cut])
        self.assertCorrupt(b"")
        self.assertCorrupt(bytes([0x01, 0x05, 0x00, 0xFA, 0xFF]) + b"hel")

    def test_distance_before_start(self):
        sym, n = fixed_code(257)  # length 3
        self.assertCorrupt(bits_to_bytes([(1, 1), (1, 2), (sym, n), (0, 5)]))

    def test_invalid_fixed_symbols(self):
        sym, n = fixed_code(286)
        self.assertCorrupt(bits_to_bytes([(1, 1), (1, 2), (sym, n)] + [(0, 8)] * 4))
        lit, ln = fixed_code(65)
        lsym, lln = fixed_code(257)
        self.assertCorrupt(bits_to_bytes([(1, 1), (1, 2), (lit, ln), (lsym, lln),
                                          (D._reverse(30, 5), 5)] + [(0, 8)] * 4))

    def test_over_subscribed_code_lengths(self):
        fields = [(1, 1), (2, 2), (0, 5), (0, 5), (15, 4)] + [(1, 3)] * 19
        self.assertCorrupt(bits_to_bytes(fields + [(0, 8)] * 8))

    def test_incomplete_code_length_code(self):
        fields = [(1, 1), (2, 2), (0, 5), (0, 5), (0, 4), (1, 3), (0, 3), (0, 3), (0, 3)]
        self.assertCorrupt(bits_to_bytes(fields + [(0, 8)] * 8))

    def test_missing_end_of_block(self):
        lit = [1, 1] + [0] * 255
        self.assertCorrupt(bits_to_bytes(dynamic_header(lit, [1, 1]) + [(0, 8)] * 4))

    def test_incomplete_literal_tree(self):
        lit = [2, 2] + [0] * 254 + [2]  # three codes of length 2: incomplete
        self.assertCorrupt(bits_to_bytes(dynamic_header(lit, [1, 1]) + [(0, 8)] * 4))

    def test_over_subscribed_literal_tree(self):
        lit = [1, 1] + [0] * 254 + [1]
        self.assertCorrupt(bits_to_bytes(dynamic_header(lit, [1, 1]) + [(0, 8)] * 4))

    def test_repeat_without_previous(self):
        cl = [0] * 19
        cl[16] = 1
        cl[0] = 1
        fields = [(1, 1), (2, 2), (0, 5), (0, 5), (15, 4)] + [(cl[D.CODELEN_ORDER[i]], 3) for i in range(19)]
        enc = D._encode_table(cl)
        fields += [enc[16], (0, 2)]
        self.assertCorrupt(bits_to_bytes(fields + [(0, 8)] * 8))

    def test_code_lengths_overrun(self):
        cl = [0] * 19
        cl[18] = 1
        cl[1] = 1
        fields = [(1, 1), (2, 2), (0, 5), (0, 5), (15, 4)] + [(cl[D.CODELEN_ORDER[i]], 3) for i in range(19)]
        enc = D._encode_table(cl)
        fields += [enc[18], (127, 7), enc[18], (127, 7), enc[18], (127, 7)]  # 3 x 138 > 258
        self.assertCorrupt(bits_to_bytes(fields + [(0, 8)] * 8))

    def test_single_distance_code_is_allowed(self):
        lit = [0] * 286
        lit[ord("a")] = 1
        lit[256] = 2
        lit[257] = 2   # length 3
        dist = [1] + [0] * 29  # single distance code (distance 1), incomplete but legal
        lit_enc = D._encode_table(lit[:258])
        fields = dynamic_header(lit[:258], dist)
        fields += [lit_enc[ord("a")], lit_enc[257], (0, 1), lit_enc[256]]
        self.assertEqual(D.inflate(bits_to_bytes(fields), BIG), b"aaaa")

    def test_random_garbage_never_hangs(self):
        rng = random.Random(5)
        for _ in range(300):
            blob = bytes(rng.getrandbits(8) for _ in range(rng.randint(0, 200)))
            try:
                D.inflate(blob, 1 << 20)
            except (CorruptFile, LimitExceeded):
                pass


class ZlibWrapperTests(unittest.TestCase):
    def test_header_errors(self):
        good = zlib.compress(b"payload")
        with self.assertRaises(CorruptFile):
            D.zlib_decompress(b"\x78", BIG)
        with self.assertRaises(CorruptFile):
            D.zlib_decompress(b"\x78\x00" + good[2:], BIG)
        with self.assertRaises(CorruptFile):
            D.zlib_decompress(b"\x79" + good[1:], BIG)
        with self.assertRaises(UnsupportedVariant):
            D.zlib_decompress(b"\x78\x20" + bytes(8), BIG)

    def test_adler_mismatch_and_missing(self):
        good = bytearray(zlib.compress(b"payload" * 20))
        bad = bytes(good[:-1]) + bytes([good[-1] ^ 1])
        with self.assertRaises(CorruptFile):
            D.zlib_decompress(bad, BIG)
        with self.assertRaises(CorruptFile):
            D.zlib_decompress(bytes(good[:-2]), BIG)

    def test_bomb_guard(self):
        bomb = zlib.compress(bytes(5_000_000), 9)
        with self.assertRaises(LimitExceeded):
            D.zlib_decompress(bomb, 1_000_000)
        stored = D.zlib_compress(bytes(200_000), 0)
        with self.assertRaises(LimitExceeded):
            D.zlib_decompress(stored, 100_000)
        self.assertEqual(len(D.zlib_decompress(bomb, 5_000_000)), 5_000_000)


class HuffmanTests(unittest.TestCase):
    def test_length_limit_and_kraft(self):
        fib = [1, 1]
        while len(fib) < 30:
            fib.append(fib[-1] + fib[-2])
        for limit in (7, 15):
            lengths = D.huffman_lengths(fib, limit)
            self.assertLessEqual(max(lengths), limit)
            self.assertEqual(sum(2.0 ** -l for l in lengths if l), 1.0)

    def test_single_symbol_padded(self):
        lengths = D.huffman_lengths([0, 0, 5], 15)
        self.assertEqual(sorted(lengths), [0, 1, 1])


if __name__ == "__main__":
    unittest.main()
