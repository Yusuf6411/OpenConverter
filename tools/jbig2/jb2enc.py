"""A small JBIG2 test encoder: the inverse of the decoder, used to make fixtures that
independent decoders (pdfium, MuPDF) can check."""
import struct, random

QE = [(0x5601,1,1,1),(0x3401,2,6,0),(0x1801,3,9,0),(0x0AC1,4,12,0),(0x0521,5,29,0),(0x0221,38,33,0),
(0x5601,7,6,1),(0x5401,8,14,0),(0x4801,9,14,0),(0x3801,10,14,0),(0x3001,11,17,0),(0x2401,12,18,0),
(0x1C01,13,20,0),(0x1601,29,21,0),(0x5601,15,14,1),(0x5401,16,14,0),(0x5101,17,15,0),(0x4801,18,16,0),
(0x3801,19,17,0),(0x3401,20,18,0),(0x3001,21,19,0),(0x2801,22,19,0),(0x2401,23,20,0),(0x2201,24,21,0),
(0x1C01,25,22,0),(0x1801,26,23,0),(0x1601,27,24,0),(0x1401,28,25,0),(0x1201,29,26,0),(0x1101,30,27,0),
(0x0AC1,31,28,0),(0x09C1,32,29,0),(0x08A1,33,30,0),(0x0521,34,31,0),(0x0441,35,32,0),(0x02A1,36,33,0),
(0x0221,37,34,0),(0x0141,38,35,0),(0x0111,39,36,0),(0x0085,40,37,0),(0x0049,41,38,0),(0x0025,42,39,0),
(0x0015,43,40,0),(0x0009,44,41,0),(0x0005,45,42,0),(0x0001,45,43,0),(0x5601,46,46,0)]

class Cx:
    __slots__=('i','m')
    def __init__(s): s.i=0; s.m=0

class MQ:
    def __init__(s):
        s.a=0x8000; s.c=0; s.ct=12; s.out=bytearray([0]); s.bp=0
    def _byteout(s):
        o=s.out
        if o[s.bp]==0xFF:
            s.bp+=1; o.append(0); o[s.bp]=(s.c>>20)&0xFF; s.c&=0xFFFFF; s.ct=7
        elif s.c<0x8000000:
            s.bp+=1; o.append(0); o[s.bp]=(s.c>>19)&0xFF; s.c&=0x7FFFF; s.ct=8
        else:
            o[s.bp]+=1
            if o[s.bp]==0xFF:
                s.c&=0x7FFFFFF
                s.bp+=1; o.append(0); o[s.bp]=(s.c>>20)&0xFF; s.c&=0xFFFFF; s.ct=7
            else:
                s.bp+=1; o.append(0); o[s.bp]=(s.c>>19)&0xFF; s.c&=0x7FFFF; s.ct=8
    def _renorm(s):
        while True:
            s.a<<=1; s.c<<=1; s.ct-=1
            if s.ct==0: s._byteout()
            if s.a&0x8000: break
    def encode(s,cx,d):
        qe,nm,nl,sw=QE[cx.i]
        if d==cx.m:
            s.a-=qe
            if s.a&0x8000==0:
                if s.a<qe: s.a=qe
                else: s.c+=qe
                cx.i=nm; s._renorm()
            else: s.c+=qe
        else:
            s.a-=qe
            if s.a<qe: s.c+=qe
            else: s.a=qe
            if sw: cx.m=1-cx.m
            cx.i=nl; s._renorm()
    def flush(s):
        t=s.c+s.a; s.c|=0xFFFF
        if s.c>=t: s.c-=0x8000
        s.c<<=s.ct; s._byteout(); s.c<<=s.ct; s._byteout()
        if s.out[s.bp]!=0xFF:
            s.bp+=1; s.out.append(0xFF)
        s.out.append(0xAC)
        return bytes(s.out[1:])

def newcx(n): return [Cx() for _ in range(n)]

class IA:
    def __init__(s): s.cx=newcx(512)
    def enc(s,mq,v):
        prev=[1]
        def bit(b):
            mq.encode(s.cx[prev[0]],b)
            p=prev[0]
            prev[0]=((p<<1)|b) if p<256 else ((((p<<1)|b)&511)|256)
        if v is None: sgn=1; m=0
        else: sgn=1 if v<0 else 0; m=abs(v)
        bit(sgn)
        if m<4: bit(0); n=2; off=0
        elif m<20: bit(1);bit(0); n=4; off=4
        elif m<84: bit(1);bit(1);bit(0); n=6; off=20
        elif m<340: bit(1);bit(1);bit(1);bit(0); n=8; off=84
        elif m<4436: bit(1);bit(1);bit(1);bit(1);bit(0); n=12; off=340
        else: bit(1);bit(1);bit(1);bit(1);bit(1); n=32; off=4436
        x=m-off
        for i in range(n-1,-1,-1): bit((x>>i)&1)

class IAID:
    def __init__(s,n): s.n=n; s.cx=newcx(1<<(n+1))
    def enc(s,mq,v):
        prev=1
        for i in range(s.n-1,-1,-1):
            b=(v>>i)&1
            mq.encode(s.cx[prev],b); prev=(prev<<1)|b

# ---- bitmaps: list of rows, each a list of 0/1
def blank(w,h,v=0): return [[v]*w for _ in range(h)]
def getp(b,x,y):
    if y<0 or y>=len(b) or x<0: return 0
    r=b[y]
    return r[x] if x<len(r) else 0

GEN_OFFS={
0:lambda a:[(-1,0),(-2,0),(-3,0),(-4,0),a[0],(2,-1),(1,-1),(0,-1),(-1,-1),(-2,-1),a[1],a[2],(1,-2),(0,-2),(-1,-2),a[3]],
1:lambda a:[(-1,0),(-2,0),(-3,0),a[0],(2,-1),(1,-1),(0,-1),(-1,-1),(-2,-1),(2,-2),(1,-2),(0,-2),(-1,-2)],
2:lambda a:[(-1,0),(-2,0),a[0],(1,-1),(0,-1),(-1,-1),(-2,-1),(1,-2),(0,-2),(-1,-2)],
3:lambda a:[(-1,0),(-2,0),(-3,0),(-4,0),a[0],(1,-1),(0,-1),(-1,-1),(-2,-1),(-3,-1)]}
DEF_AT={0:[(3,-1),(-3,-1),(2,-2),(-2,-2)],1:[(3,-1)],2:[(2,-1)],3:[(2,-1)]}
SLTP={0:0x9B25,1:0x0795,2:0x00E5,3:0x0195}
NCX={0:1<<16,1:1<<13,2:1<<10,3:1<<10}

def enc_generic(mq,cx,bm,w,h,tpl,tpgd=False,at=None,skip=None):
    at=at or DEF_AT[tpl]
    offs=GEN_OFFS[tpl](at)
    ltp=0
    for y in range(h):
        if tpgd:
            same = (bm[y]==bm[y-1]) if y>0 else not any(bm[0])
            sl=(1 if same else 0)^ltp
            mq.encode(cx[SLTP[tpl]],sl); ltp=1 if same else 0
            if same: continue
        for x in range(w):
            if skip and skip[y][x]: continue
            c=0
            for i,(dx,dy) in enumerate(offs): c|=getp(bm,x+dx,y+dy)<<i
            mq.encode(cx[c],bm[y][x])

def enc_refine(mq,cx,bm,w,h,ref,dx,dy,tpl,tpgr=False,at=((-1,-1),(-1,-1))):
    if tpl==0:
        spec=[(0,-1,0),(0,1,-1),(0,0,-1),(0,)+tuple(at[0]),(1,1,1),(1,0,1),(1,-1,1),(1,1,0),(1,0,0),(1,-1,0),(1,1,-1),(1,0,-1),(1,)+tuple(at[1])]
        sl=1<<8
    else:
        spec=[(0,-1,0),(0,1,-1),(0,0,-1),(0,-1,-1),(1,1,1),(1,0,1),(1,1,0),(1,0,0),(1,-1,0),(1,0,-1)]
        sl=1<<7
    ltp=0
    def typical_pix(x,y):
        rx,ry=x-dx,y-dy
        v=getp(ref,rx,ry)
        for j in (-1,0,1):
            for i in (-1,0,1):
                if getp(ref,rx+i,ry+j)!=v: return None
        return v
    for y in range(h):
        if tpgr:
            ok=True
            for x in range(w):
                t=typical_pix(x,y)
                if t is not None and t!=bm[y][x]: ok=False;break
            mq.encode(cx[sl],(1 if ok else 0)^ltp); ltp=1 if ok else 0
        for x in range(w):
            if tpgr and ltp and typical_pix(x,y) is not None: continue
            c=0
            for i,(r,ox,oy) in enumerate(spec):
                p=getp(ref,x-dx+ox,y-dy+oy) if r else getp(bm,x+ox,y+oy)
                c|=p<<i
            mq.encode(cx[c],bm[y][x])

# ---- segments
def seg(num,typ,refs,page,data,length=None):
    flags=typ|(0x40 if page>255 else 0)
    h=struct.pack('>IB',num,flags)
    assert len(refs)<=4
    h+=bytes([(len(refs)<<5)])+bytes(refs)
    h+=struct.pack('>I',page) if page>255 else bytes([page])
    h+=struct.pack('>I',len(data) if length is None else length)
    return h+data

def region_info(w,h,x,y,op=0): return struct.pack('>IIIIB',w,h,x,y,op)
def page_info(w,h,default=0,stripe=0):
    return struct.pack('>IIIIBH',w,h,0,0,default<<2,stripe)
def at_bytes(at): return b''.join(struct.pack('bb',a,b) for a,b in at)

def generic_seg(num,bm,w,h,x,y,tpl=0,tpgd=False,at=None,op=0,typ=38,page=1,unknown=False):
    mq=MQ(); cx=newcx(NCX[tpl]); at=at or DEF_AT[tpl]
    enc_generic(mq,cx,bm,w,h,tpl,tpgd,at)
    d=region_info(w,h,x,y,op)+bytes([(tpl<<1)|(8 if tpgd else 0)])+at_bytes(at)+mq.flush()
    if unknown: d+=struct.pack('>I',h)
    return seg(num,typ,[],page,d,0xFFFFFFFF if unknown else None)

def refine_seg(num,bm,ref,w,h,x,y,tpl=0,tpgr=False,at=((-1,-1),(-1,-1)),refs=(),typ=42,page=1,op=0):
    mq=MQ(); cx=newcx(1<<13 if tpl==0 else 1<<10)
    enc_refine(mq,cx,bm,w,h,ref,0,0,tpl,tpgr,at)
    d=region_info(w,h,x,y,op)+bytes([tpl|(2 if tpgr else 0)])+(at_bytes(at) if tpl==0 else b'')+mq.flush()
    return seg(num,typ,list(refs),page,d)

def ceil_log2(n):
    b=0
    while (1<<b)<n: b+=1
    return b

def symdict_seg(num,insyms,newsyms,export,tpl=0,at=None,refs=(),page=0,ctx_used=False,ctx_kept=False):
    """newsyms: list of bitmaps (rows), must be sorted by height."""
    at=at or DEF_AT[tpl]
    mq=MQ(); cx=newcx(NCX[tpl]); iadh=IA(); iadw=IA(); iaex=IA()
    i=0; hc=0
    n=len(newsyms)
    while i<n:
        h=len(newsyms[i]); iadh.enc(mq,h-hc); hc=h
        sw=0
        while i<n and len(newsyms[i])==h:
            b=newsyms[i]; w=len(b[0]) if h else 0
            iadw.enc(mq,w-sw); sw=w
            enc_generic(mq,cx,b,w,h,tpl,False,at)
            i+=1
        iadw.enc(mq,None)
    total=len(insyms)+n
    flags=export if export is not None else [False]*len(insyms)+[True]*n
    idx=0;cur=False
    while idx<total:
        run=0
        while idx+run<total and flags[idx+run]==cur: run+=1
        iaex.enc(mq,run); idx+=run; cur=not cur
    nex=sum(flags)
    f=(tpl<<10)|(0x100 if ctx_used else 0)|(0x200 if ctx_kept else 0)
    d=struct.pack('>H',f)+(at_bytes(at) if tpl==0 else at_bytes(at[:1]))+struct.pack('>II',nex,n)+mq.flush()
    return seg(num,0,list(refs),page,d)

def text_seg(num,w,h,x,y,insts,syms,refs,strips_log=0,corner=1,transposed=False,comb=0,defpix=0,dsoff=0,op=0,
             refine=None,rtpl=0,rat=((-1,-1),(-1,-1)),typ=6,page=1):
    """insts: list of (id, x, y[, refinement]) top-left coords of the (possibly refined) bitmap. refinement = bitmap or None"""
    mq=MQ(); S=1<<strips_log
    codelen=ceil_log2(len(syms))
    iadt=IA();iafs=IA();iads=IA();iait=IA();iari=IA();iardw=IA();iardh=IA();iardx=IA();iardy=IA()
    iaid=IAID(codelen); gr=newcx(1<<13 if rtpl==0 else 1<<10)
    right=corner>=2; bottom=corner in (0,2)
    items=[]
    for it in insts:
        sid,ix,iy=it[0],it[1],it[2]
        rb=it[3] if len(it)>3 else None
        bw,bh=(len(rb[0]),len(rb)) if rb is not None else (len(syms[sid][0]),len(syms[sid]))
        if not transposed:
            si=ix+(bw-1 if right else 0); ti=iy+(bh-1 if bottom else 0)
        else:
            si=iy+(bh-1 if bottom else 0); ti=ix+(bw-1 if right else 0)
        items.append((ti//S,si,ti%S,sid,rb,bw,bh))
    items.sort(key=lambda t:(t[0],t[1]))
    iadt.enc(mq,0)
    stript=0; firsts=0; k=0
    while k<len(items):
        st=items[k][0]
        iadt.enc(mq,st-stript); stript=st
        first=True; curs=0
        while k<len(items) and items[k][0]==st:
            _,si,ct,sid,rb,bw,bh=items[k]
            pre=(bw-1) if ((not transposed and right) or (transposed and bottom)) else 0
            spre=si-pre
            if first: iafs.enc(mq,spre-firsts); firsts=spre; first=False
            else: iads.enc(mq,spre-curs-dsoff)
            if S>1: iait.enc(mq,ct)
            iaid.enc(mq,sid)
            if refine is not None:
                iari.enc(mq,1 if rb is not None else 0)
                if rb is not None:
                    ob=syms[sid]; ow,oh=len(ob[0]),len(ob)
                    rdw=bw-ow; rdh=bh-oh
                    iardw.enc(mq,rdw); iardh.enc(mq,rdh); iardx.enc(mq,0); iardy.enc(mq,0)
                    enc_refine(mq,gr,rb,bw,bh,ob,rdw//2,rdh//2,rtpl,False,rat)
            post=(bw-1) if ((not transposed and not right) or (transposed and not bottom)) else 0
            curs=si+post
            k+=1
        iads.enc(mq,None)
    f=(1 if False else 0)|((2 if refine is not None else 0))|(strips_log<<2)|(corner<<4)|((0x40) if transposed else 0)|(comb<<7)|(defpix<<9)|((dsoff&0x1F)<<10)|(rtpl<<15)
    d=region_info(w,h,x,y,op)+struct.pack('>H',f)
    if refine is not None and rtpl==0: d+=at_bytes(rat)
    d+=struct.pack('>I',len(insts))+mq.flush()
    return seg(num,typ,list(refs),page,d)

def pattern_seg(num,patterns,pw,ph,tpl=0,page=0):
    gm=len(patterns)-1; w=(gm+1)*pw
    coll=[sum((p[y] for p in patterns),[]) for y in range(ph)]
    at=[(-pw,0),(-3,-1),(2,-2),(-2,-2)]
    mq=MQ(); cx=newcx(NCX[tpl])
    enc_generic(mq,cx,coll,w,ph,tpl,False,at)
    d=bytes([tpl<<1,pw,ph])+struct.pack('>I',gm)+mq.flush()
    return seg(num,16,[],page,d)

def halftone_seg(num,pat_num,patterns,grid,w,h,x,y,gx,gy,rx,ry,tpl=0,comb=0,defpix=0,skip_on=False,op=0,typ=22,page=1):
    """grid: gh rows of gw pattern indices"""
    gh=len(grid); gw=len(grid[0]); npat=len(patterns)
    bpp=ceil_log2(npat); pw=len(patterns[0][0]); ph=len(patterns[0])
    skip=None
    if skip_on:
        skip=blank(gw,gh)
        for m in range(gh):
            for n in range(gw):
                xx=(gx+m*ry+n*rx)>>8; yy=(gy+m*rx-n*ry)>>8
                if xx+pw<=0 or xx>=w or yy+ph<=0 or yy>=h: skip[m][n]=1; grid[m][n]=0
    at=[(3 if tpl<=1 else 2,-1),(-3,-1),(2,-2),(-2,-2)]
    mq=MQ(); cx=newcx(NCX[tpl])
    planes=[]
    for j in range(bpp):
        planes.append([[ (grid[m][n]>>j)&1 for n in range(gw)] for m in range(gh)])
    prev=None
    for j in range(bpp-1,-1,-1):
        pl=planes[j]
        if prev is not None:
            pl=[[pl[m][n]^prev[m][n] for n in range(gw)] for m in range(gh)]
        # prev holds the true plane j+1 (gray coded relative)
        prev=planes[j]
        enc_generic(mq,cx,pl,gw,gh,tpl,False,at,skip)
    d=region_info(w,h,x,y,op)+bytes([(tpl<<1)|(8 if skip_on else 0)|(comb<<4)|(defpix<<7)])
    d+=struct.pack('>IIiiHH',gw,gh,gx,gy,rx,ry)+mq.flush()
    return seg(num,typ,[pat_num],page,d)

def file_wrap(segs,sequential=True):
    return b'\x97JB2\r\n\x1a\n'+bytes([1 if sequential else 0])+struct.pack('>I',1)+b''.join(segs)

def pdf_wrap(w,h,stream,globals_=None):
    objs=[]
    objs.append(b'<</Type/Catalog/Pages 2 0 R>>')
    objs.append(b'<</Type/Pages/Kids[3 0 R]/Count 1>>')
    objs.append(('<</Type/Page/Parent 2 0 R/MediaBox[0 0 %d %d]/Contents 5 0 R/Resources<</XObject<</Im0 4 0 R>>>>>>'%(w,h)).encode())
    parms=b'<</JBIG2Globals 6 0 R>>' if globals_ is not None else b''
    img=('<</Type/XObject/Subtype/Image/Width %d/Height %d/ColorSpace/DeviceGray/BitsPerComponent 1/Filter/JBIG2Decode'%(w,h)).encode()
    if parms: img+=b'/DecodeParms'+parms
    img+=('/Length %d>>\nstream\n'%len(stream)).encode()+stream+b'\nendstream'
    objs.append(img)
    c=('q %d 0 0 %d 0 0 cm /Im0 Do Q'%(w,h)).encode()
    objs.append(('<</Length %d>>\nstream\n'%len(c)).encode()+c+b'\nendstream')
    if globals_ is not None:
        objs.append(('<</Length %d>>\nstream\n'%len(globals_)).encode()+globals_+b'\nendstream')
    out=bytearray(b'%PDF-1.5\n'); offs=[]
    for i,o in enumerate(objs):
        offs.append(len(out)); out+=('%d 0 obj\n'%(i+1)).encode()+o+b'\nendobj\n'
    x=len(out)
    out+=('xref\n0 %d\n0000000000 65535 f \n'%(len(objs)+1)).encode()
    for o in offs: out+=('%010d 00000 n \n'%o).encode()
    out+=('trailer\n<</Size %d/Root 1 0 R>>\nstartxref\n%d\n%%%%EOF\n'%(len(objs)+1,x)).encode()
    return bytes(out)

def pack(bm):
    w=len(bm[0]); out=bytearray()
    for r in bm:
        for i in range(0,w,8):
            v=0
            for k in range(8):
                v=(v<<1)|(r[i+k] if i+k<w else 0)
            out.append(v)
    return bytes(out)
