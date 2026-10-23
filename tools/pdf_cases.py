#!/usr/bin/env python3
"""Generates the PDFs used by tools/pdf_interop.py into a directory (reportlab, LibreOffice, pikepdf)."""
import io, os, subprocess, sys, shutil, tempfile
from reportlab.pdfgen import canvas
from reportlab.lib.colors import CMYKColor, Color
from reportlab.lib.pagesizes import A4, letter
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from PIL import Image
import pikepdf

DEJAVU = '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf'
SERIF = '/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf'

def make_images():
    rgb = Image.new('RGB', (64, 48))
    for y in range(48):
        for x in range(64):
            rgb.putpixel((x, y), (x * 4, y * 5, 128 + (x ^ y) % 100))
    gray = rgb.convert('L')
    rgba = Image.new('RGBA', (60, 60))
    for y in range(60):
        for x in range(60):
            rgba.putpixel((x, y), (255 - x * 4, y * 4, 80, 255 if (x - 30) ** 2 + (y - 30) ** 2 < 800 else 40))
    pal = rgb.convert('P', palette=Image.ADAPTIVE, colors=16)
    one = Image.new('1', (64, 48))
    for y in range(48):
        for x in range(64):
            one.putpixel((x, y), (x // 8 + y // 8) % 2)
    return rgb, gray, rgba, pal, one

def text_std(path):
    c = canvas.Canvas(path, pagesize=A4)
    c.setTitle('Standard fonts'); c.setAuthor('Test Author'); c.setSubject('subject')
    y = 800
    for name in ['Helvetica', 'Helvetica-Bold', 'Helvetica-Oblique', 'Times-Roman', 'Times-BoldItalic', 'Courier', 'Courier-Bold']:
        c.setFont(name, 14); c.drawString(50, y, f'{name}: The quick brown fox jumps over the lazy dog 0123456789'); y -= 24
    c.setFont('Symbol', 14); c.drawString(50, y, 'abgdez'); y -= 24
    c.setFont('ZapfDingbats', 14); c.drawString(50, y, 'abcdef'); y -= 24
    c.setFont('Helvetica', 12)
    t = c.beginText(50, y); t.setCharSpace(2); t.textLine('character spacing 2'); t.setCharSpace(0); t.setWordSpace(10); t.textLine('word spacing is ten here'); t.setWordSpace(0)
    t.setHorizScale(60); t.textLine('horizontal scale 60'); t.setHorizScale(100); t.setRise(5); t.textLine('rise 5'); t.setRise(0)
    t.setTextRenderMode(1); t.setFillColorRGB(1, 0, 0); t.textLine('stroke mode'); t.setTextRenderMode(2); t.setStrokeColorRGB(0, 0, 1); t.textLine('fill and stroke'); t.setTextRenderMode(0)
    c.drawText(t); y -= 160
    c.saveState(); c.translate(300, 400); c.rotate(30); c.setFont('Times-Roman', 20); c.setFillColorRGB(0.2, 0.5, 0.2); c.drawString(0, 0, 'Rotated text 30 degrees'); c.restoreState()
    c.setFont('Helvetica', 10)
    para = 'Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. ' * 4
    t = c.beginText(50, 300)
    import textwrap
    for line in textwrap.wrap(para, 90): t.textLine(line)
    c.drawText(t)
    c.showPage()
    c.setFont('Helvetica-Bold', 24); c.drawString(50, 780, 'Second page heading')
    c.setFont('Helvetica', 11); c.drawString(50, 750, 'Body text on page two.')
    c.bookmarkPage('p2'); c.addOutlineEntry('Second page', 'p2', 0)
    c.linkURL('https://example.com', (50, 740, 250, 760), relative=0)
    c.showPage(); c.save()

def graphics(path):
    c = canvas.Canvas(path, pagesize=letter)
    c.setLineWidth(3); c.setStrokeColorRGB(0.8, 0.1, 0.1); c.setFillColorRGB(0.9, 0.9, 0.2)
    c.rect(50, 650, 120, 80, fill=1)
    c.setLineJoin(1); c.setLineCap(1); c.setDash([6, 3]); c.line(200, 650, 400, 730); c.setDash([])
    p = c.beginPath(); p.moveTo(50, 600); p.curveTo(100, 650, 150, 550, 200, 600); p.lineTo(200, 500); p.close()
    c.setFillColor(CMYKColor(0.8, 0.2, 0, 0.1)); c.drawPath(p, fill=1, stroke=1)
    p = c.beginPath(); p.circle(320, 560, 50); p.circle(320, 560, 25); c.setFillColorRGB(0.2, 0.3, 0.8); c.drawPath(p, fill=1, stroke=0, fillMode=0)
    c.saveState(); p = c.beginPath(); p.circle(120, 400, 60); c.clipPath(p, stroke=0)
    c.setFillColorRGB(0.1, 0.7, 0.4); c.rect(50, 340, 70, 120, fill=1, stroke=0); c.setFillColorRGB(0.9, 0.3, 0.1); c.rect(120, 340, 70, 120, fill=1, stroke=0); c.restoreState()
    c.setFillAlpha(0.5); c.setFillColorRGB(1, 0, 0); c.circle(300, 400, 50, fill=1); c.setFillColorRGB(0, 0, 1); c.circle(340, 400, 50, fill=1); c.setFillAlpha(1)
    c.setStrokeColorRGB(0, 0, 0); c.setLineWidth(0.3); c.roundRect(50, 250, 200, 60, 15)
    c.setLineWidth(0); c.line(50, 240, 250, 240)
    c.saveState(); c.translate(400, 300); c.scale(1.5, 0.7); c.rotate(20); c.setFillColorRGB(0.5, 0.2, 0.7); c.rect(0, 0, 60, 60, fill=1, stroke=0); c.restoreState()
    for i in range(6):
        c.setFillColorRGB(i / 5, 0.3, 1 - i / 5); c.rect(50 + i * 30, 150, 28, 60, fill=1, stroke=0)
    c.setFillGray(0.5); c.rect(50, 80, 100, 40, fill=1, stroke=0)
    c.showPage(); c.save()

def gradients(path):
    c = canvas.Canvas(path, pagesize=letter)
    from reportlab.lib.colors import red, blue, yellow, green, white, black
    c.linearGradient(50, 600, 250, 700, (red, blue), extend=False)
    c.linearGradient(300, 600, 500, 700, (yellow, green, blue), (0, 0.3, 1), extend=True)
    c.radialGradient(150, 400, 80, (white, black), extend=False)
    c.radialGradient(400, 400, 80, (red, yellow, white), (0, 0.5, 1), extend=True)
    c.showPage(); c.save()

def images(path):
    rgb, gray, rgba, pal, one = make_images()
    d = os.path.dirname(path)
    rgb.save(f'{d}/_i_rgb.png'); gray.save(f'{d}/_i_gray.png'); rgba.save(f'{d}/_i_rgba.png'); pal.save(f'{d}/_i_pal.png'); one.save(f'{d}/_i_one.png'); rgb.save(f'{d}/_i.jpg', quality=85)
    c = canvas.Canvas(path, pagesize=letter)
    c.setFillColorRGB(0.9, 0.95, 1); c.rect(0, 0, 612, 792, fill=1, stroke=0)
    c.drawImage(f'{d}/_i_rgb.png', 50, 650, 128, 96)
    c.drawImage(f'{d}/_i_gray.png', 200, 650, 128, 96)
    c.drawImage(f'{d}/_i.jpg', 350, 650, 128, 96)
    c.drawImage(f'{d}/_i_rgba.png', 50, 500, 120, 120, mask='auto')
    c.drawImage(f'{d}/_i_pal.png', 200, 500, 128, 96)
    c.drawImage(f'{d}/_i_one.png', 350, 500, 128, 96)
    c.saveState(); c.translate(150, 350); c.rotate(25); c.drawImage(f'{d}/_i_rgb.png', 0, 0, 128, 96); c.restoreState()
    c.save()

def ttf(path):
    pdfmetrics.registerFont(TTFont('DejaVu', DEJAVU)); pdfmetrics.registerFont(TTFont('DejaVuSerif', SERIF))
    c = canvas.Canvas(path, pagesize=A4)
    c.setFont('DejaVu', 16); c.drawString(50, 780, 'Embedded TrueType: Привет мир, Γειά σου, café, naïve, ñ, ü, €')
    c.setFont('DejaVuSerif', 14); c.drawString(50, 750, 'Serif subset: ﬁ ligature chars, “quotes” — dashes … and ½ ¼')
    c.setFont('DejaVu', 10); c.drawString(50, 720, 'Mixed 日本語 will be missing glyphs but text is kept.')
    c.save()

def lo_docs(outdir, tmp):
    html = '''<html><head><meta charset="utf-8"><title>LO test</title></head><body>
<h1>Heading One</h1><p>This is a paragraph with <b>bold</b>, <i>italic</i> and <u>underlined</u> text. It is long enough to wrap across more than one line in the page so that line grouping is exercised by extraction.</p>
<h2>List</h2><ul><li>First item</li><li>Second item</li><li>Third item</li></ul><ol><li>One</li><li>Two</li></ol>
<h2>Table</h2><table border="1"><tr><th>Name</th><th>Qty</th><th>Price</th></tr><tr><td>Apple</td><td>3</td><td>1.20</td></tr><tr><td>Banana</td><td>12</td><td>0.50</td></tr></table>
<p style="font-family:'DejaVu Serif'">Serif paragraph — with “smart quotes”, café and Ελληνικά.</p>
<p style="font-family:Courier New,monospace">monospace fixed width text</p>
</body></html>'''
    open(f'{tmp}/doc.html', 'w').write(html)
    for f in ['doc.html']:
        subprocess.run(['soffice', '--headless', '--convert-to', 'pdf:writer_web_pdf_Export', '--outdir', outdir, f'{tmp}/{f}'], capture_output=True, timeout=180)
    if os.path.exists(f'{outdir}/doc.pdf'):
        os.rename(f'{outdir}/doc.pdf', f'{outdir}/lo_html.pdf')
    open(f'{tmp}/plain.txt', 'w').write('\n'.join(f'Line {i}: the quick brown fox jumps over the lazy dog' for i in range(120)))
    subprocess.run(['soffice', '--headless', '--convert-to', 'pdf', '--outdir', outdir, f'{tmp}/plain.txt'], capture_output=True, timeout=180)
    if os.path.exists(f'{outdir}/plain.pdf'):
        os.rename(f'{outdir}/plain.pdf', f'{outdir}/lo_text_multipage.pdf')

def variants(outdir):
    src = f'{outdir}/text_std.pdf'
    with pikepdf.open(src) as p:
        p.save(f'{outdir}/v_objstm.pdf', object_stream_mode=pikepdf.ObjectStreamMode.generate, compress_streams=True)
        p.save(f'{outdir}/v_linear.pdf', linearize=True)
        p.save(f'{outdir}/v_rc4_128.pdf', encryption=pikepdf.Encryption(owner='own', user='', R=3, aes=False, metadata=False))
        p.save(f'{outdir}/v_aes128.pdf', encryption=pikepdf.Encryption(owner='own', user='', R=4))
        p.save(f'{outdir}/v_aes256.pdf', encryption=pikepdf.Encryption(owner='own', user='', R=6))
        p.save(f'{outdir}/v_aes256_pw.pdf', encryption=pikepdf.Encryption(owner='own', user='secret', R=6))
        p.save(f'{outdir}/v_rc4_40_pw.pdf', encryption=pikepdf.Encryption(owner='own', user='secret', R=2, aes=False, metadata=False))
    b = open(src, 'rb').read()
    open(f'{outdir}/v_junk_prefix.pdf', 'wb').write(b'junk before the header\n' * 3 + b)
    open(f'{outdir}/v_no_xref.pdf', 'wb').write(b[: b.rindex(b'startxref')])
    i = b.index(b'xref')
    open(f'{outdir}/v_bad_xref.pdf', 'wb').write(b[:i] + b'xref\n0 3\n' + b'0000000000 65535 f \n' * 3 + b[b.index(b'trailer'):])
    open(f'{outdir}/v_truncated.pdf', 'wb').write(b[: len(b) * 3 // 4])


def raw_pdf(path, objs, extra_page='', mediabox='0 0 200 200', root_extra=''):
    """objs: dict num -> bytes body (without 'n 0 obj'); object 1 = catalog, 2 = pages, 3 = page."""
    out = b'%PDF-1.7\n'
    offs = {}
    for n in sorted(objs):
        offs[n] = len(out)
        out += f'{n} 0 obj\n'.encode() + objs[n] + b'\nendobj\n'
    x = len(out)
    top = max(objs) + 1
    out += f'xref\n0 {top}\n'.encode() + b'0000000000 65535 f \n'
    for n in range(1, top):
        out += (f'{offs[n]:010d} 00000 n \n' if n in offs else '0000000000 65535 f \n').encode()
    out += f'trailer\n<< /Size {top} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n'.encode()
    open(path, 'wb').write(out)

def stm(d, data):
    if isinstance(data, str): data = data.encode('latin1')
    return b'<< ' + d.encode() + f' /Length {len(data)} >>\nstream\n'.encode() + data + b'\nendstream'

def features(out):
    base = {1: b'<< /Type /Catalog /Pages 2 0 R >>', 2: b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>'}
    def page(res, content, extra=''):
        o = dict(base)
        o[3] = f'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources {res} /Contents 4 0 R {extra} >>'.encode()
        o[4] = stm('', content)
        return o
    # tiling patterns (coloured and uncoloured) and a shading pattern
    o = page('<< /Pattern << /P1 5 0 R /P2 6 0 R /P3 7 0 R >> /ColorSpace << /PCS [/Pattern /DeviceRGB] >> >>',
             '/Pattern cs /P1 scn 10 110 80 80 re f /PCS cs 1 0 0 /P2 scn 110 110 80 80 re f /Pattern cs /P3 scn 10 10 180 80 re f')
    o[5] = stm('/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>', '0 0 1 rg 0 0 5 5 re f 1 1 0 rg 5 5 5 5 re f')
    o[6] = stm('/Type /Pattern /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 8 8] /XStep 8 /YStep 8 /Resources << >>', '4 4 3 0 360 arc 0 0 m 8 8 l 1 w S 0 4 m 8 4 l S')
    o[7] = b'<< /Type /Pattern /PatternType 2 /Matrix [1 0 0 1 10 10] /Shading << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 180 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >> >>'
    raw_pdf(f'{out}/f_patterns.pdf', o)
    # transparency: constant alpha, blend mode, group form, luminosity soft mask
    o = page('<< /ExtGState << /A << /ca 0.5 /CA 0.5 >> /M << /BM /Multiply >> /S << /SMask << /Type /Mask /S /Luminosity /G 5 0 R >> >> >> /XObject << /F 6 0 R >> >>',
             '1 0 0 rg 20 20 100 100 re f /A gs 0 0 1 rg 70 70 100 100 re f q /M gs 1 1 0 rg 30 120 120 60 re f Q q /S gs 0 0.6 0 rg 0 0 200 200 re f Q /A gs /F Do')
    o[5] = stm('/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Group << /S /Transparency /CS /DeviceGray >> /Resources << /Shading << /S1 << /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 200 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >> >> >>', '/S1 sh')
    o[6] = stm('/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Group << /S /Transparency /I true >> /Resources << >>', '1 0.5 0 rg 100 20 60 60 re f 0 0.5 1 rg 130 40 60 60 re f')
    raw_pdf(f'{out}/f_transparency.pdf', o)
    # Type 3 font with coloured and uncoloured glyphs
    o = page('<< /Font << /T3 5 0 R >> >>', 'BT /T3 40 Tf 20 120 Td 1 0 0 rg (ab) Tj ET BT /T3 20 Tf 20 60 Td 0 0.5 0 rg (abab) Tj ET')
    o[5] = b'<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /sq 6 0 R /tri 7 0 R >> /Encoding << /Type /Encoding /Differences [97 /sq /tri] >> /FirstChar 97 /LastChar 98 /Widths [1000 1000] >>'
    o[6] = stm('', '1000 0 0 0 800 800 d1 0 0 800 800 re f')
    o[7] = stm('', '1000 0 d0 0 0 1 rg 0 0 m 800 0 l 400 800 l f')
    raw_pdf(f'{out}/f_type3.pdf', o)
    # inline images, rotation, CMYK/Lab/Indexed, dashes
    o = page('<< >>', 'q 80 0 0 80 10 110 cm BI /W 4 /H 4 /BPC 8 /CS /G ID ' + ''.join(chr(v) for v in range(0, 256, 16)) + ' EI Q '
             'q 60 0 0 60 110 110 cm BI /W 2 /H 2 /BPC 8 /CS /RGB /F /AHx ID FF0000 00FF00 0000FF FFFF00> EI Q '
             '0 1 1 0 k 10 10 50 50 re f 1 0 0 0 K 4 w [6 3] 0 d 70 10 m 190 90 l S 0 0 0 1 k 100 20 80 20 re f',
             '/Rotate 90')
    raw_pdf(f'{out}/f_inline_rotate.pdf', o)
    # mesh shadings
    import struct
    def mesh4():
        d = b''
        for (x, y, r, g, b) in [(10, 10, 255, 0, 0), (190, 10, 0, 255, 0), (100, 190, 0, 0, 255)]:
            d += bytes([0]) + struct.pack('>HH', int(x / 200 * 65535), int(y / 200 * 65535)) + bytes([r, g, b])
        return d
    o = page('<< /Shading << /M 5 0 R /FB 6 0 R >> >>', '/M sh q 0 0 60 60 re W n /FB sh Q')
    o[5] = stm('/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 16 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 200 0 200 0 1 0 1 0 1]', mesh4())
    o[6] = stm('/ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] /Matrix [60 0 0 60 0 0] /Function 7 0 R', '') .replace(b'/Length 0', b'/Length 0')
    o[6] = b'<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] /Matrix [60 0 0 60 0 0] /Function 7 0 R >>'
    o[7] = stm('/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]', '{ 2 copy mul }')
    raw_pdf(f'{out}/f_mesh.pdf', o)
    # annotations, optional content, text render modes, clip text
    o = page('<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >> >> /Properties << /OC1 8 0 R /OC2 9 0 R >> >>',
             '/OC /OC1 BDC 1 0 0 rg 10 10 40 40 re f EMC /OC /OC2 BDC 0 0 1 rg 60 10 40 40 re f EMC '
             'BT /F1 48 Tf 7 Tr 10 120 Td (CLIP) Tj ET 0 0.6 0 rg 0 100 200 40 re f 1 0 0 rg 0 140 200 40 re f',
             '/Annots [10 0 R]')
    o[1] = b'<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [8 0 R 9 0 R] /D << /OFF [9 0 R] >> >> >>'
    o[8] = b'<< /Type /OCG /Name (visible) >>'
    o[9] = b'<< /Type /OCG /Name (hidden) >>'
    o[10] = b'<< /Type /Annot /Subtype /Stamp /Rect [120 10 180 60] /AP << /N 11 0 R >> /F 4 >>'
    o[11] = stm('/Type /XObject /Subtype /Form /BBox [0 0 30 30] /Resources << >>', '1 0 1 rg 0 0 30 30 re f 0 g 5 5 20 20 re f')
    raw_pdf(f'{out}/f_misc.pdf', o)

def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    tmp = tempfile.mkdtemp()
    text_std(f'{out}/text_std.pdf'); graphics(f'{out}/graphics.pdf'); gradients(f'{out}/gradients.pdf'); images(f'{out}/images.pdf'); ttf(f'{out}/ttf.pdf')
    lo_docs(out, tmp); variants(out); features(out)
    for f in os.listdir(out):
        if f.startswith('_i'): os.remove(f'{out}/{f}')
    print(sorted(os.listdir(out)))
main()
