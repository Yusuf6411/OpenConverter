import json
import unittest

from openconverter.codecs import delimited, jsontable
from openconverter.codecs import text as T
from openconverter.converters.table_converter import convert_table
from openconverter.converters.text_converter import convert_text
from openconverter.errors import CorruptFile, LimitExceeded, UnsupportedVariant
from openconverter.limits import Limits
from openconverter.models.table import Table

SAMPLE = "Grüße, naïve café – “quotes” €\nline two\n"


class TextDecodeTests(unittest.TestCase):
    def test_bom_variants(self):
        for enc, prefix, py in (("utf-8-bom", b"\xef\xbb\xbf", "utf-8"), ("utf-16le-bom", b"\xff\xfe", "utf-16-le"),
                                ("utf-16be-bom", b"\xfe\xff", "utf-16-be")):
            text, detected = T.decode_text(prefix + SAMPLE.encode(py))
            self.assertEqual((text, detected), (SAMPLE, enc))

    def test_bomless_utf16(self):
        for enc, py in (("utf-16le", "utf-16-le"), ("utf-16be", "utf-16-be")):
            self.assertEqual(T.decode_text("plain ascii text\r\nmore".encode(py)), ("plain ascii text\r\nmore", enc))

    def test_utf8_then_fallback(self):
        self.assertEqual(T.decode_text(SAMPLE.encode("utf-8")), (SAMPLE, "utf-8"))
        cp = "café – €".encode("cp1252")
        self.assertEqual(T.decode_text(cp), ("café – €", "windows-1252"))
        self.assertEqual(T.decode_text(cp, "latin-1"), (cp.decode("latin-1"), "latin-1"))
        self.assertEqual(T.decode_text(b"\x81\x8d"), ("\x81\x8d", "latin-1"))

    def test_invalid_explicit_fallback(self):
        with self.assertRaises(CorruptFile):
            T.decode_text(b"\xff\xfe\x00\xd8", "windows-1252")  # BOM + lone surrogate
        with self.assertRaises(UnsupportedVariant):
            T.decode_text(b"\xff\xfe\x00\x00abcd")

    def test_line_endings(self):
        self.assertEqual(T.line_ending_kind("a\r\nb\r\n"), "crlf")
        self.assertEqual(T.line_ending_kind("a\nb"), "lf")
        self.assertEqual(T.line_ending_kind("a\rb\nc"), "mixed")
        self.assertEqual(T.normalise_newlines("a\r\nb\rc\nd", "lf"), "a\nb\nc\nd")
        self.assertEqual(T.normalise_newlines("a\r\nb\rc\nd", "crlf"), "a\r\nb\r\nc\r\nd")


class TextConvertTests(unittest.TestCase):
    def test_utf8_to_utf16le_crlf(self):
        out, notes, _ = convert_text(SAMPLE.encode("utf-8"), {"to_encoding": "utf-16le", "newline": "crlf"})
        self.assertEqual(out, SAMPLE.replace("\n", "\r\n").encode("utf-16-le"))
        self.assertTrue(any("LF to CRLF" in n for n in notes))

    def test_bom_targets(self):
        out, _, _ = convert_text(b"hi", {"to_encoding": "utf-16be-bom"})
        self.assertEqual(out, b"\xfe\xff\x00h\x00i")
        out, _, _ = convert_text(b"hi", {"to_encoding": "utf-8-bom"})
        self.assertEqual(out, b"\xef\xbb\xbfhi")

    def test_unencodable_error_and_replace(self):
        with self.assertRaises(UnsupportedVariant) as cm:
            convert_text("price €5 and ☃".encode("utf-8"), {"to_encoding": "latin-1"})
        self.assertIn("2 character", str(cm.exception))
        out, notes, _ = convert_text("price €5 and ☃☃".encode("utf-8"), {"to_encoding": "latin-1",
                                                                          "unencodable": "replace"})
        self.assertEqual(out, b"price ?5 and ??")
        self.assertTrue(any("Replaced 3" in n for n in notes))
        out, _, _ = convert_text("€".encode("utf-8"), {"to_encoding": "windows-1252"})
        self.assertEqual(out, b"\x80")

    def test_mixed_preserved_and_identity(self):
        data = b"a\r\nb\nc"
        out, notes, _ = convert_text(data, {})
        self.assertEqual(out, data)
        self.assertTrue(any("mixed" in n for n in notes))
        self.assertTrue(any("No change" in n for n in notes))


class CsvParseTests(unittest.TestCase):
    def p(self, text, d=","):
        return delimited.parse(text, d)

    def test_rfc4180_edge_cases(self):
        self.assertEqual(self.p('a,b,c\r\n1,"x, y","say ""hi"""\r\n'), [["a", "b", "c"], ["1", "x, y", 'say "hi"']])
        self.assertEqual(self.p('a,"line1\nline2",c\n'), [["a", "line1\nline2", "c"]])
        self.assertEqual(self.p('a,,c\n,,\n'), [["a", "", "c"], ["", "", ""]])
        self.assertEqual(self.p('a,b,\n1,2,'), [["a", "b", ""], ["1", "2", ""]])
        self.assertEqual(self.p('a,b\n1\n1,2,3'), [["a", "b"], ["1"], ["1", "2", "3"]])
        self.assertEqual(self.p('""\n"",""'), [[""], ["", ""]])
        self.assertEqual(self.p("a,b"), [["a", "b"]])
        self.assertEqual(self.p("\ufeffa,b\n"), [["a", "b"]])
        self.assertEqual(self.p("a\n\n\nb\r\n\r\n"), [["a"], ["b"]])
        self.assertEqual(self.p("a\rb\r"), [["a"], ["b"]])
        self.assertEqual(self.p(""), [])

    def test_other_delimiters(self):
        self.assertEqual(self.p("a\tb\t\"c\td\"\n", "\t"), [["a", "b", "c\td"]])
        self.assertEqual(self.p("a;b|c\n", ";"), [["a", "b|c"]])

    def test_unterminated_quote(self):
        with self.assertRaises(CorruptFile):
            self.p('a,"open\n')

    def test_limits(self):
        with self.assertRaises(LimitExceeded):
            delimited.parse("a,b,c\n" * 10, ",", Limits(max_table_cells=20))
        with self.assertRaises(LimitExceeded):
            delimited.parse("x" * 100, ",", Limits(max_csv_field=50))

    def test_detect_delimiter(self):
        self.assertEqual(delimited.detect_delimiter("a;b;c\n1;2;3\n"), ";")
        self.assertEqual(delimited.detect_delimiter("a\tb\n1\t2\n"), "\t")
        self.assertEqual(delimited.detect_delimiter('a,b\n"x;y;z",2\n'), ",")
        self.assertEqual(delimited.detect_delimiter("a|b|c\n1|2|3"), "|")
        self.assertEqual(delimited.detect_delimiter("single"), ",")

    def test_write_quoting_and_round_trip(self):
        t = Table(["h1", "h,2"], [["plain", 'q"uote'], ["multi\nline", ""], ["only"]])
        out = delimited.write(t, ",")
        self.assertEqual(out, 'h1,"h,2"\r\nplain,"q""uote"\r\n"multi\nline",\r\nonly,\r\n')
        self.assertEqual(delimited.parse(out, ","), [["h1", "h,2"], ["plain", 'q"uote'], ["multi\nline", ""],
                                                     ["only", ""]])
        self.assertEqual(delimited.write(Table(None, [[""]]), ",", "lf"), '""\n')


class JsonTableTests(unittest.TestCase):
    def test_array_of_objects(self):
        t = jsontable.read('[{"a": 1, "b": true}, {"c": null, "a": "x"}, {"b": 1.5}]')
        self.assertEqual(t.header, ["a", "b", "c"])
        self.assertEqual(t.rows, [["1", "true", ""], ["x", "", ""], ["", "1.5", ""]])
        self.assertTrue(t.metadata["coerced_scalars"])

    def test_array_of_arrays_and_empty(self):
        t = jsontable.read('[["a", "b"], [1, false]]')
        self.assertIsNone(t.header)
        self.assertEqual(t.rows, [["a", "b"], ["1", "false"]])
        self.assertEqual(jsontable.read("[]").rows, [])

    def test_rejections(self):
        for bad, exc in (('{"a": 1}', UnsupportedVariant), ('[{"a": {"b": 1}}]', UnsupportedVariant),
                         ('[[1, [2]]]', UnsupportedVariant), ('[1, 2]', UnsupportedVariant),
                         ('[{"a": 1}, [1]]', UnsupportedVariant), ('[{"a": 1', CorruptFile)):
            with self.assertRaises(exc, msg=bad):
                jsontable.read(bad)
        with self.assertRaises(UnsupportedVariant) as cm:
            jsontable.read('[{"a": 1}, {"a": [1]}]')
        self.assertIn("row 2", str(cm.exception))

    def test_write_dedup_header(self):
        notes = []
        out = jsontable.write(Table(["a", "a", ""], [["1", "2", "3"]]), 0, notes=notes)
        self.assertEqual(json.loads(out), [{"a": "1", "a_2": "2", "column_3": "3"}])
        self.assertTrue(notes)

    def test_indent_and_ascii(self):
        t = Table(["k"], [["é"]])
        self.assertEqual(jsontable.write(t, 0), '[{"k":"é"}]\n')
        self.assertEqual(jsontable.write(t, 0, ensure_ascii=True), '[{"k":"\\u00e9"}]\n')
        self.assertIn('\n    {', jsontable.write(t, 4))


class TableConvertTests(unittest.TestCase):
    CSV = 'name,notes\r\nAda,"likes, commas\nand newlines"\r\nBob,"say ""hi"""\r\n'.encode("utf-8")

    def test_all_six_pairs_round_trip(self):
        tsv, _, _ = convert_table(self.CSV, "csv", "tsv", {})
        js, _, _ = convert_table(self.CSV, "csv", "json", {})
        self.assertEqual(json.loads(js), [{"name": "Ada", "notes": "likes, commas\nand newlines"},
                                          {"name": "Bob", "notes": 'say "hi"'}])
        self.assertEqual(convert_table(tsv, "tsv", "csv", {})[0], self.CSV)
        self.assertEqual(convert_table(js, "json", "csv", {})[0], self.CSV)
        self.assertEqual(convert_table(tsv, "tsv", "json", {})[0], js)
        self.assertEqual(convert_table(js, "json", "tsv", {})[0], tsv)

    def test_csv_options(self):
        out, _, _ = convert_table(b"a;b\n1;2\n", "csv", "csv", {"delimiter": "pipe", "newline": "lf", "bom": True})
        self.assertEqual(out, b"\xef\xbb\xbfa|b\n1|2\n")
        out, _, _ = convert_table(b"a,b\n1,2\n", "csv", "json", {"has_header": False, "indent": 0})
        self.assertEqual(out, b'[["a","b"],["1","2"]]\n')
        out, _, _ = convert_table(b"a;b\n1;2\n", "csv", "json", {"source_delimiter": "comma", "indent": 0})
        self.assertEqual(out, b'[{"a;b":"1;2"}]\n')

    def test_ragged_padded_and_noted(self):
        out, notes, _ = convert_table(b"a,b,c\n1\n", "csv", "csv", {"newline": "lf"})
        self.assertEqual(out, b"a,b,c\n1,,\n")
        self.assertTrue(any("padded" in n for n in notes))

    def test_bom_and_cp1252_input(self):
        out, _, _ = convert_table(b"\xef\xbb\xbfa,b\n1,2\n", "csv", "json", {"indent": 0})
        self.assertEqual(out, b'[{"a":"1","b":"2"}]\n')
        out, notes, _ = convert_table("x\ncafé\n".encode("cp1252"), "csv", "json", {"indent": 0})
        self.assertEqual(json.loads(out), [{"x": "café"}])
        self.assertTrue(any("Windows-1252" in n for n in notes))

    def test_json_scalars_noted(self):
        _, notes, _ = convert_table(b'[{"n": 1}]', "json", "csv", {})
        self.assertTrue(any("as text" in n for n in notes))
        _, notes, _ = convert_table(b'[{"n": "1"}]', "json", "csv", {})
        self.assertFalse(any("as text" in n for n in notes))


if __name__ == "__main__":
    unittest.main()
