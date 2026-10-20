#!/usr/bin/env python3
"""Checks XLSX and ODS reading and writing against openpyxl, xlsxwriter and LibreOffice.

Usage: python3 tools/sheet_interop.py   (after `cargo build`; needs openpyxl, xlsxwriter,
and optionally soffice for the LibreOffice round trips)."""
import csv, datetime, io, os, shutil, subprocess, sys, tempfile

BIN = os.path.join(os.path.dirname(__file__), "..", "target", "debug", "openconverter")
tmp = tempfile.mkdtemp()
fails = 0


def check(cond, name):
    global fails
    print(("ok   " if cond else "FAIL ") + name)
    if not cond:
        fails += 1


def run(*args):
    r = subprocess.run([BIN, *args], capture_output=True, text=True)
    return r


def to_rows(path, *extra):
    out = os.path.join(tmp, "o.csv")
    r = run("convert", path, out, "--overwrite", "--set", "has_header=false", *extra)
    if r.returncode != 0:
        print(r.stderr)
        return None
    with open(out, newline="", encoding="utf-8") as f:
        return list(csv.reader(f))


import openpyxl, xlsxwriter

# --- reading xlsx written by other tools
p = os.path.join(tmp, "xw.xlsx")
wb = xlsxwriter.Workbook(p)
ws = wb.add_worksheet("First")
date = wb.add_format({"num_format": "yyyy-mm-dd"})
dt = wb.add_format({"num_format": "yyyy-mm-dd hh:mm:ss"})
tm = wb.add_format({"num_format": "hh:mm"})
pct = wb.add_format({"num_format": "0.0%"})
ws.write_row(0, 0, ["text", "int", "float", "bool"])
ws.write_row(1, 0, ["a&b <c>", 42, 3.14159, True])
ws.write_datetime(2, 0, datetime.datetime(2020, 2, 29), date)
ws.write_datetime(2, 1, datetime.datetime(1999, 12, 31, 23, 59, 59), dt)
ws.write_datetime(2, 2, datetime.datetime(1900, 1, 1, 6, 30), tm)
ws.write_number(2, 3, 0.256, pct)
ws.write_string(3, 0, "  padded  ")
ws.write_string(3, 1, "line1\nline2")
ws.write_string(3, 2, "日本語 ünï")
ws.write_formula(3, 3, "=B2*2", None, 84)
ws.write_blank(4, 0, None)
ws.write_number(5, 5, 1e-7)
ws.write_number(5, 6, 123456789012345.0)
ws.merge_range("A8:C8", "merged")
ws2 = wb.add_worksheet("Sheet Two")
ws2.write(0, 0, "only")
wb.close()
rows = to_rows(p)
check(rows is not None and rows[0][:4] == ["text", "int", "float", "bool"], "xlsxwriter: header row")
check(rows[1][:4] == ["a&b <c>", "42", "3.14159", "TRUE"], "xlsxwriter: text, numbers, booleans")
check(rows[2][0] == "2020-02-29", "xlsxwriter: date")
check(rows[2][1] == "1999-12-31 23:59:59", "xlsxwriter: date and time")
check(rows[2][2] == "06:30:00", "xlsxwriter: time of day")
check(rows[2][3] == "0.256", "xlsxwriter: percentage keeps its value")
check(rows[3][:3] == ["  padded  ", "line1\nline2", "日本語 ünï"], "xlsxwriter: spaces, newlines, unicode")
check(rows[3][3] == "84", "xlsxwriter: formula cached value")
check(rows[5][5] == "1E-07" or rows[5][5] == "0.0000001" or rows[5][5].lower() == "1e-7", f"xlsxwriter: small number {rows[5][5]!r}")
check(rows[5][6] == "123456789012345", "xlsxwriter: large integer")
check(rows[7][0] == "merged" and rows[7][1] == "", "xlsxwriter: merged cell")
rows2 = to_rows(p, "--set", "sheet=Sheet Two")
check(rows2 == [["only"]], "sheet selected by name")
check(to_rows(p, "--set", "sheet=2") == [["only"]], "sheet selected by number")

# --- 1904 date system and inline strings via openpyxl
p = os.path.join(tmp, "op.xlsx")
w = openpyxl.Workbook()
w.epoch = openpyxl.utils.datetime.CALENDAR_MAC_1904
s = w.active
s["A1"] = datetime.date(2021, 7, 4)
s["B1"] = "x"
w.save(p)
check(to_rows(p) == [["2021-07-04", "x"]], "1904 date system")

# --- our writer, checked by openpyxl and LibreOffice
src = os.path.join(tmp, "in.csv")
with open(src, "w", encoding="utf-8", newline="") as f:
    f.write('name,qty,price,when,flag,zip,note\r\n')
    f.write('Alpha,3,1.25,2024-01-05,TRUE,00123,"line\nbreak"\r\n')
    f.write('Beta,-7,0.5,2024-02-29 13:45:10,false,98765,  lead\r\n')
    f.write('Gamma,,1e3,not a date,yes,12345678901234567890,"a & <b>"\r\n')
for ext in ("xlsx", "ods"):
    out = os.path.join(tmp, "out." + ext)
    r = run("convert", src, out, "--overwrite")
    check(r.returncode == 0, f"csv -> {ext}")
    back = to_rows(out)
    check(back is not None and back[0][0] == "name", f"{ext}: header survives")
    check(back[1] == ["Alpha", "3", "1.25", "2024-01-05", "TRUE", "00123", "line\nbreak"], f"{ext}: typed values read back as written")
    check(back[2][3] == "2024-02-29 13:45:10" and back[2][4] == "FALSE", f"{ext}: datetime and boolean")
    check(back[3][5] == "12345678901234567890" and back[3][2] == "1e3", f"{ext}: long digits and exponent stay text")
    if shutil.which("soffice"):
        lo = os.path.join(tmp, "lo_" + ext)
        subprocess.run(["soffice", "--headless", "--convert-to", "csv", "--outdir", lo, out], capture_output=True, timeout=180)
        got = os.path.join(lo, "out.csv")
        check(os.path.exists(got), f"LibreOffice opens our {ext}")
        if os.path.exists(got):
            lines = list(csv.reader(open(got, encoding="utf-8", newline="")))
            check(lines[1][:3] == ["Alpha", "3", "1.25"] and lines[1][5] == "00123", f"LibreOffice reads our {ext} cells")
            check(lines[1][3] in ("2024-01-05", "01/05/24", "1/5/24") or "2024" in lines[1][3] or "24" in lines[1][3], f"LibreOffice sees a date in our {ext}: {lines[1][3]}")
    if ext == "xlsx":
        sheet = openpyxl.load_workbook(out).active
        check(sheet["B2"].value == 3 and isinstance(sheet["D2"].value, datetime.datetime), "openpyxl reads our xlsx types")
        check(sheet["A1"].font.b and sheet.freeze_panes == "A2", "header is bold and frozen")
        check(sheet["F2"].value == "00123", "leading zeros stay text")

# --- multi-sheet workbook round trips both ways
for a, b in (("xlsx", "ods"), ("ods", "xlsx")):
    one = os.path.join(tmp, "multi." + a)
    if a == "xlsx":
        w = openpyxl.Workbook()
        w.active.title = "Alpha"
        w.active.append(["h1", "h2"])
        w.active.append([1, 2])
        w.create_sheet("Beta").append(["only", "beta"])
        w.save(one)
    else:
        mid = os.path.join(tmp, "mid.xlsx")
        w = openpyxl.Workbook()
        w.active.title = "Alpha"
        w.active.append(["h1", "h2"])
        w.active.append([1, 2])
        w.create_sheet("Beta").append(["only", "beta"])
        w.save(mid)
        run("convert", mid, one, "--overwrite")
    conv = os.path.join(tmp, "conv." + b)
    run("convert", one, conv, "--overwrite")
    r1 = to_rows(conv, "--set", "sheet=Beta")
    check(r1 == [["only", "beta"]], f"{a} -> {b} keeps the second sheet by name")

# --- damaged input never panics
bad = os.path.join(tmp, "bad.xlsx")
data = open(os.path.join(tmp, "out.xlsx"), "rb").read()
open(bad, "wb").write(data[: len(data) // 2])
r = run("convert", bad, os.path.join(tmp, "bad.csv"), "--overwrite")
check(r.returncode != 0 and "panick" not in r.stderr.lower(), "truncated xlsx is an error, not a crash")

shutil.rmtree(tmp, ignore_errors=True)
print("failures:", fails)
sys.exit(1 if fails else 0)
