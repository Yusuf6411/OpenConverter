import sys, os, io
sys.path.insert(0,os.path.dirname(__file__))
from jb2enc import *
import fitz, pypdfium2 as pdfium
import numpy as np
def oracle_mupdf(pdf):
    doc=fitz.open(stream=pdf,filetype='pdf')
    xref=[x[0] for x in doc.get_page_images(0)][0]
    pm=fitz.Pixmap(doc,xref)
    a=np.frombuffer(pm.samples,dtype=np.uint8).reshape(pm.h,pm.w,pm.n)[:,:,0]
    return (a<128).astype(np.uint8)
def oracle_pdfium(pdf,w,h):
    d=pdfium.PdfDocument(pdf); pg=d[0]
    bm=pg.render(scale=1.0,grayscale=True,may_draw_forms=False)
    a=bm.to_numpy()
    if a.ndim==3: a=a[:,:,0]
    return (a<128).astype(np.uint8)
