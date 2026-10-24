import sys, os, struct
sys.path.insert(0,os.path.dirname(__file__))
import numpy as np
from jb2enc import *
import jbcases, jbrun_lib
out=sys.argv[1]; os.makedirs(out,exist_ok=True)
for f in os.listdir(out): os.remove(os.path.join(out,f))
for n,f in jbcases.CASES.items():
    c=f(); w,h=c['w'],c['h']
    pdf=pdf_wrap(w,h,c['stream'],c['globals'])
    m=jbrun_lib.oracle_mupdf(pdf); p=jbrun_lib.oracle_pdfium(pdf,w,h)
    e=np.array(c['img'],dtype=np.uint8)
    if (m==e).all() and (p==e).all(): ref=e; why='both agree'
    elif (m==e).all() or (p==e).all(): ref=e; why='one oracle agrees with the encoder (%s)'%('mupdf' if (m==e).all() else 'pdfium')
    elif (m==p).all(): ref=m; why='oracles agree, encoder differs'
    else: print(n,'NO CONSENSUS'); continue
    open(f'{out}/{n}.jb2','wb').write(c['stream'])
    if c['globals'] is not None: open(f'{out}/{n}.glob','wb').write(c['globals'])
    open(f'{out}/{n}.ref','wb').write(struct.pack('>II',w,h)+pack(ref.tolist()))
    if why!='both agree': print(n,why)

# standalone files, sequential and random access, built from some of the streams above
def split(stream):
    pos=0; out=[]
    while pos<len(stream):
        st=pos
        num,flags=struct.unpack('>IB',stream[pos:pos+5]); pos+=5
        cnt=stream[pos]>>5; pos+=1+cnt
        pos+=4 if flags&0x40 else 1
        ln=struct.unpack('>I',stream[pos:pos+4])[0]; pos+=4
        out.append((stream[st:pos],stream[pos:pos+ln])); pos+=ln
    return out
for n in ('gen_t0_tp','text_basic','halftone','striped_unknown_height','sd_refagg_t0'):
    c=jbcases.CASES[n](); parts=split(c['stream'])
    eof=seg(100,51,[],0,b'')
    seq=b'\x97JB2\r\n\x1a\n'+bytes([1])+struct.pack('>I',1)+c['stream']+eof
    ra=b'\x97JB2\r\n\x1a\n'+bytes([0])+struct.pack('>I',1)+b''.join(h for h,_ in parts)+split(eof)[0][0]+b''.join(d for _,d in parts)
    ref=open(f'{out}/{n}.ref','rb').read()
    for kind,data in (('seq',seq),('ra',ra)):
        open(f'{out}/file_{kind}_{n}.jbf','wb').write(data)
        open(f'{out}/file_{kind}_{n}.ref','wb').write(ref)
