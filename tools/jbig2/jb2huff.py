import struct, io
from jb2enc import *

# Standard tables B.1 to B.15: (prefix length, range length, range low) lines; the last two lines
# before an optional OOB line are the lower and upper range lines. Table: (lines, has_oob).
TABLES = {
    1: ([(1, 4, 0), (2, 8, 16), (3, 16, 272), (0, 32, -1), (3, 32, 65808)], False),
    2: ([(1, 0, 0), (2, 0, 1), (3, 0, 2), (4, 3, 3), (5, 6, 11), (0, 32, -1), (6, 32, 75), (6, 0, 0)], True),
    3: ([(8, 8, -256), (1, 0, 0), (2, 0, 1), (3, 0, 2), (4, 3, 3), (5, 6, 11), (8, 32, -257), (7, 32, 75), (6, 0, 0)], True),
    4: ([(1, 0, 1), (2, 0, 2), (3, 0, 3), (4, 3, 4), (5, 6, 12), (0, 32, -1), (5, 32, 76)], False),
    5: ([(7, 8, -255), (1, 0, 1), (2, 0, 2), (3, 0, 3), (4, 3, 4), (5, 6, 12), (7, 32, -256), (6, 32, 76)], False),
    6: ([(5, 10, -2048), (4, 9, -1024), (4, 8, -512), (4, 7, -256), (5, 6, -128), (5, 5, -64), (4, 5, -32), (2, 7, 0), (3, 7, 128), (3, 8, 256), (4, 9, 512), (4, 10, 1024), (6, 32, -2049), (6, 32, 2048)], False),
    7: ([(4, 9, -1024), (3, 8, -512), (4, 7, -256), (5, 6, -128), (5, 5, -64), (4, 5, -32), (4, 5, 0), (5, 5, 32), (5, 6, 64), (4, 7, 128), (3, 8, 256), (3, 9, 512), (3, 10, 1024), (5, 32, -1025), (5, 32, 2048)], False),
    8: ([(8, 3, -15), (9, 1, -7), (8, 1, -5), (9, 0, -3), (7, 0, -2), (4, 0, -1), (2, 1, 0), (5, 0, 2), (6, 0, 3), (3, 4, 4), (6, 1, 20), (4, 4, 22), (4, 5, 38), (5, 6, 70), (5, 7, 134), (6, 7, 262), (7, 8, 390), (6, 10, 646), (9, 32, -16), (9, 32, 1670), (2, 0, 0)], True),
    9: ([(8, 4, -31), (9, 2, -15), (8, 2, -11), (9, 1, -7), (7, 1, -5), (4, 1, -3), (3, 1, -1), (3, 1, 1), (5, 1, 3), (6, 1, 5), (3, 5, 7), (6, 2, 39), (4, 5, 43), (4, 6, 75), (5, 7, 139), (5, 8, 267), (6, 8, 523), (7, 9, 779), (6, 11, 1291), (9, 32, -32), (9, 32, 3339), (2, 0, 0)], True),
    10: ([(7, 4, -21), (8, 0, -5), (7, 0, -4), (5, 0, -3), (2, 2, -2), (5, 0, 2), (6, 0, 3), (7, 0, 4), (8, 0, 5), (2, 6, 6), (5, 5, 70), (6, 5, 102), (6, 6, 134), (6, 7, 198), (6, 8, 326), (6, 9, 582), (6, 10, 1094), (7, 11, 2118), (8, 32, -22), (8, 32, 4166), (2, 0, 0)], True),
    11: ([(1, 0, 1), (2, 1, 2), (4, 0, 4), (4, 1, 5), (5, 1, 7), (5, 2, 9), (6, 2, 13), (7, 2, 17), (7, 3, 21), (7, 4, 29), (7, 5, 45), (7, 6, 77), (0, 32, -1), (7, 32, 141)], False),
    12: ([(1, 0, 1), (2, 0, 2), (3, 1, 3), (5, 0, 5), (5, 1, 6), (6, 1, 8), (7, 0, 10), (7, 1, 11), (7, 2, 13), (7, 3, 17), (7, 4, 25), (8, 5, 41), (8, 32, 73), (0, 32, -1), (0, 32, 0)], False),
    13: ([(1, 0, 1), (3, 0, 2), (4, 0, 3), (5, 0, 4), (4, 1, 5), (3, 3, 7), (6, 1, 15), (6, 2, 17), (6, 3, 21), (6, 4, 29), (6, 5, 45), (7, 6, 77), (0, 32, -1), (7, 32, 141)], False),
    14: ([(3, 0, -2), (3, 0, -1), (1, 0, 0), (3, 0, 1), (3, 0, 2), (0, 32, -1), (0, 32, 3)], False),
    15: ([(7, 4, -24), (6, 2, -8), (5, 1, -4), (4, 0, -2), (3, 0, -1), (1, 0, 0), (3, 0, 1), (4, 0, 2), (5, 1, 3), (6, 2, 5), (7, 4, 9), (7, 32, -25), (7, 32, 25)], False),
}

class Huff:
    def __init__(s,n):
        lines,oob=TABLES[n] if isinstance(n,int) else n
        s.lines=lines; s.oob=len(lines)-1 if oob else None
        tail=len(lines)-(1 if oob else 0)
        s.lower=tail-2; s.upper=tail-1
        mx=max(l[0] for l in lines)
        cnt=[0]*(mx+2)
        for l in lines: cnt[l[0]]+=1
        cnt[0]=0
        s.codes={}
        first=0
        for ln in range(1,mx+1):
            first=(first+cnt[ln-1])<<1
            cur=first
            for i,l in enumerate(lines):
                if l[0]==ln: s.codes[i]=(cur,ln); cur+=1
    def enc(s,w,v):
        """w: BitWriter. v=None for OOB"""
        if v is None:
            c,l=s.codes[s.oob]; w.put(c,l); return
        for i,(pref,rng,low) in enumerate(s.lines):
            if pref==0 or i==s.oob: continue
            if i==s.lower:
                if v<=low: c,l=s.codes[i]; w.put(c,l); w.put(low-v,32); return
                continue
            if rng==32:
                if v>=low: c,l=s.codes[i]; w.put(c,l); w.put(v-low,32); return
                continue
            if low<=v<low+(1<<rng):
                c,l=s.codes[i]; w.put(c,l); w.put(v-low,rng); return
        raise ValueError('no code for %r'%v)

class BitWriter:
    def __init__(s): s.b=[]
    def put(s,v,n):
        for i in range(n-1,-1,-1): s.b.append((v>>i)&1)
    def align(s):
        while len(s.b)%8: s.b.append(0)
    def bytes_(s):
        s.align(); out=bytearray()
        for i in range(0,len(s.b),8):
            v=0
            for k in range(8): v=(v<<1)|s.b[i+k]
            out.append(v)
        return bytes(out)
    def raw(s,bs):
        s.align()
        for x in bs: s.put(x,8)

def g4(bm):
    """T.6 data for a bitmap (1 = black), via libtiff in Pillow."""
    from PIL import Image
    import numpy as np
    a=np.array(bm,dtype=np.uint8)
    im=Image.fromarray((a*255).astype(np.uint8)).convert('1')
    f=io.BytesIO(); im.save(f,format='TIFF',compression='group4'); d=f.getvalue()
    bo='<' if d[:2]==b'II' else '>'
    ifd=struct.unpack(bo+'I',d[4:8])[0]
    n=struct.unpack(bo+'H',d[ifd:ifd+2])[0]
    off=cnt=None
    for i in range(n):
        tag,typ,c,val=struct.unpack(bo+'HHII',d[ifd+2+12*i:ifd+14+12*i])
        if typ==3 and c==1: val=struct.unpack(bo+'H',d[ifd+2+12*i+8:ifd+2+12*i+10])[0]
        if tag==273: off=val
        if tag==279: cnt=val
    return d[off:off+cnt]

def symdict_huff_seg(num,insyms,newsyms,mmr=False,page=0,refs=(),dht=4,dwt=2):
    w=BitWriter(); dh=Huff(dht); dw=Huff(dwt); bs=Huff(1); ex=Huff(1)
    n=len(newsyms); i=0; hc=0
    while i<n:
        h=len(newsyms[i]); dh.enc(w,h-hc); hc=h
        sw=0; j=i
        while j<n and len(newsyms[j])==h:
            wd=len(newsyms[j][0]); dw.enc(w,wd-sw); sw=wd; j+=1
        dw.enc(w,None)
        syms=newsyms[i:j]
        coll=[sum((s[y] for s in syms),[]) for y in range(h)]
        if mmr:
            data=g4(coll); bs.enc(w,len(data)); w.align(); w.raw(data)
        else:
            bs.enc(w,0); w.align(); w.raw(pack(coll))
        i=j
    total=len(insyms)+n
    flags=[False]*len(insyms)+[True]*n
    idx=0;cur=False
    while idx<total:
        run=0
        while idx+run<total and flags[idx+run]==cur: run+=1
        ex.enc(w,run); idx+=run; cur=not cur
    f=1|({4:0,5:1}[dht]<<2)|({2:0,3:1}[dwt]<<4)
    d=struct.pack('>H',f)+struct.pack('>II',n,n)+w.bytes_()
    return seg(num,0,list(refs),page,d)

def text_huff_seg(num,w_,h_,x,y,insts,syms,refs,strips_log=0,corner=1,comb=0,op=0,typ=6,page=1,lens=None,dsoff=0,
                  refine=False,rtpl=0,rat=((-1,-1),(-1,-1)),fst=6,dst=8,dtt=11,rdt=14,fs_user=None,extra_refs=()):
    S=1<<strips_log
    fs=Huff(fs_user if fs_user is not None else fst); ds=Huff(dst); dt=Huff(dtt); rdw=Huff(rdt); rdh=Huff(rdt); rdx=Huff(rdt); rdy=Huff(rdt); rsz=Huff(1)
    N=len(syms)
    if lens is None:
        cl=max(1,ceil_log2(N)); lens=[cl]*N
    # canonical codes for symbols
    mx=max(lens); cnt=[0]*(mx+2)
    for l in lens: cnt[l]+=1
    cnt[0]=0; codes={}; first=0
    for ln in range(1,mx+1):
        first=(first+cnt[ln-1])<<1; cur=first
        for i,l in enumerate(lens):
            if l==ln: codes[i]=(cur,ln); cur+=1
    grcx=newcx(1<<13 if rtpl==0 else 1<<10)
    w=BitWriter()
    # run code lengths: codes 0..31 get 5 bits, others 0
    for i in range(35): w.put(5 if i<32 else 0,4)
    for l in lens: w.put(l,5)       # run code for value l is canonical code of l with 5 bits = l
    w.align()
    right=corner>=2; bottom=corner in (0,2)
    items=[]
    for it in insts:
        sid,ix,iy=it[0],it[1],it[2]
        rb=it[3] if len(it)>3 else None
        bw,bh=(len(rb[0]),len(rb)) if rb is not None else (len(syms[sid][0]),len(syms[sid]))
        si=ix+(bw-1 if right else 0); ti=iy+(bh-1 if bottom else 0)
        items.append((ti//S,si,ti%S,sid,rb,bw,bh))
    items.sort(key=lambda t:(t[0],t[1]))
    dt.enc(w,1)
    stript=-1; firsts=0; k=0
    while k<len(items):
        st=items[k][0]; dt.enc(w,st-stript); stript=st
        first=True; curs=0
        while k<len(items) and items[k][0]==st:
            _,si,ct,sid,rb,bw,bh=items[k]
            pre=(bw-1) if right else 0
            spre=si-pre
            if first: fs.enc(w,spre-firsts); firsts=spre; first=False
            else: ds.enc(w,spre-curs-dsoff)
            if S>1: w.put(ct,strips_log)
            c,l=codes[sid]; w.put(c,l)
            if refine:
                w.put(1 if rb is not None else 0,1)
                if rb is not None:
                    ob=syms[sid]; ow,oh=len(ob[0]),len(ob)
                    rdw_=bw-ow; rdh_=bh-oh
                    rdw.enc(w,rdw_); rdh.enc(w,rdh_); rdx.enc(w,0); rdy.enc(w,0)
                    mq=MQ()
                    enc_refine(mq,grcx,rb,bw,bh,ob,rdw_//2,rdh_//2,rtpl,False,rat)
                    data=mq.flush()
                    rsz.enc(w,len(data)); w.align(); w.raw(data)
            curs=si+((bw-1) if not right else 0)
            k+=1
        ds.enc(w,None)
    f=1|(2 if refine else 0)|(strips_log<<2)|(corner<<4)|(comb<<7)|((dsoff&0x1F)<<10)|(rtpl<<15)
    rs=rdt-14
    fsel=3 if fs_user is not None else {6:0,7:1}[fst]
    hf=fsel|({8:0,9:1,10:2}[dst]<<2)|({11:0,12:1,13:2}[dtt]<<4)|(rs<<6)|(rs<<8)|(rs<<10)|(rs<<12)
    d=region_info(w_,h_,x,y,op)+struct.pack('>HH',f,hf)
    if refine and rtpl==0: d+=at_bytes(rat)
    d+=struct.pack('>I',len(insts))+w.bytes_()
    return seg(num,typ,list(refs)+list(extra_refs),page,d)

def generic_mmr_seg(num,bm,w,h,x,y,op=0,typ=38,page=1):
    d=region_info(w,h,x,y,op)+bytes([1])+g4(bm)
    return seg(num,typ,[],page,d)

def pattern_mmr_seg(num,patterns,pw,ph,page=0):
    gm=len(patterns)-1
    coll=[sum((p[y] for p in patterns),[]) for y in range(ph)]
    d=bytes([1,pw,ph])+struct.pack('>I',gm)+g4(coll)
    return seg(num,16,[],page,d)

def halftone_mmr_seg(num,pat_num,patterns,grid,w,h,x,y,gx,gy,rx,ry,comb=0,defpix=0,op=0,typ=22,page=1):
    gh=len(grid); gw=len(grid[0]); bpp=ceil_log2(len(patterns))
    planes=[[[ (grid[m][n]>>j)&1 for n in range(gw)] for m in range(gh)] for j in range(bpp)]
    data=b''; prev=None
    for j in range(bpp-1,-1,-1):
        pl=planes[j]
        cod=[[pl[m][n]^(prev[m][n] if prev else 0) for n in range(gw)] for m in range(gh)]
        prev=pl
        data+=g4(cod)
    d=region_info(w,h,x,y,op)+bytes([1|(comb<<4)|(defpix<<7)])+struct.pack('>IIiiHH',gw,gh,gx,gy,rx,ry)+data
    return seg(num,typ,[pat_num],page,d)

CUSTOM=([(2,9,-512),(2,9,0),(3,32,-513),(3,32,512)],False)
def custom_table_seg(num,page=0):
    ps,rs=2,4
    w=BitWriter()
    w.put(2,ps); w.put(9,rs)      # line -512..-1
    w.put(2,ps); w.put(9,rs)      # line 0..511
    w.put(3,ps); w.put(3,ps)      # lower, upper
    d=bytes([((ps-1)<<1)|((rs-1)<<4)])+struct.pack('>ii',-512,512)+w.bytes_()
    return seg(num,53,[],page,d)
