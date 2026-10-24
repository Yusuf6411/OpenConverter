import random, struct, sys
from jb2enc import *

def rnd_bitmap(w,h,seed,kind='blobs'):
    r=random.Random(seed)
    b=blank(w,h)
    if kind=='noise':
        for y in range(h):
            for x in range(w): b[y][x]=1 if r.random()<0.3 else 0
    else:
        for _ in range(max(1,w*h//60)):
            cx,cy=r.randrange(w),r.randrange(h); rw,rh=r.randrange(1,9),r.randrange(1,9)
            for y in range(cy,min(h,cy+rh)):
                for x in range(cx,min(w,cx+rw)): b[y][x]=1
        # a few repeated rows for TPGD
        for y in range(1,h):
            if r.random()<0.2: b[y]=list(b[y-1])
    return b

def glyphs(n,seed):
    r=random.Random(seed); out=[]
    for i in range(n):
        w=r.randrange(3,12); h=r.randrange(4,14)
        out.append(rnd_bitmap(w,h,seed*100+i,'noise' if i%2 else 'blobs'))
    out.sort(key=lambda g:(len(g),len(g[0])))
    return out

def page_stream(w,h,segs,default=0):
    return seg(0,48,[],1,page_info(w,h,default))+b''.join(segs)+seg(99,49,[],1,b'')

CASES={}
def case(name):
    def deco(f): CASES[name]=f; return f
    return deco

def mk_generic(name,tpl,tpgd,at,kind,w=61,h=47,seed=1):
    @case(name)
    def _():
        b=rnd_bitmap(w,h,seed,kind)
        s=page_stream(w,h,[generic_seg(1,b,w,h,0,0,tpl,tpgd,at)])
        return dict(w=w,h=h,img=b,stream=s,globals=None)
for tpl in range(4):
    for tp in (False,True):
        mk_generic(f'gen_t{tpl}{"_tp" if tp else ""}',tpl,tp,None,'blobs',61+tpl,47,tpl+7)
mk_generic('gen_noise',0,False,None,'noise',64,40,3)
mk_generic('gen_at0',0,False,[(-5,-1),(4,-2),(-6,0),(7,-3)] and [(-5,-1),(4,-2),(-1,-3),(6,-4)],'blobs',70,50,5)
mk_generic('gen_at1',1,False,[(-4,-2)],'blobs',70,50,6)
mk_generic('gen_big',0,True,None,'blobs',300,200,9)

@case('gen_unknown_len')
def _():
    w,h=50,30; b=rnd_bitmap(w,h,11)
    s=page_stream(w,h,[generic_seg(1,b,w,h,0,0,0,False,None,unknown=True)])
    return dict(w=w,h=h,img=b,stream=s,globals=None)

@case('gen_two_regions')
def _():
    w,h=80,60; b1=rnd_bitmap(30,20,1); b2=rnd_bitmap(40,25,2)
    img=blank(w,h)
    for (b,x,y,op) in ((b1,5,7,0),(b2,30,30,2)):
        for j,row in enumerate(b):
            for i,v in enumerate(row):
                if op==0: img[y+j][x+i]|=v
                else: img[y+j][x+i]^=v
    s=page_stream(w,h,[generic_seg(1,b1,30,20,5,7,0,False,None,0),generic_seg(2,b2,40,25,30,30,1,True,None,2)])
    return dict(w=w,h=h,img=img,stream=s,globals=None)

def place(img,b,x,y,op=0):
    for j,row in enumerate(b):
        for i,v in enumerate(row):
            if 0<=y+j<len(img) and 0<=x+i<len(img[0]):
                if op==0: img[y+j][x+i]|=v
                elif op==2: img[y+j][x+i]^=v

def mk_text(name,tpl=0,strips_log=0,corner=1,transposed=False,use_globals=False,refine=False,rtpl=0,ninst=40,seed=1):
    @case(name)
    def _():
        syms=glyphs(12,seed); w,h=120,90
        r=random.Random(seed)
        insts=[];img=blank(w,h)
        for k in range(ninst):
            sid=r.randrange(len(syms)); b=syms[sid]
            rb=None
            if refine and r.random()<0.4:
                rb=[row[:] for row in b]
                # make the refined symbol slightly bigger
                if r.random()<0.5:
                    for row in rb: row.append(r.randrange(2))
                    rb.append([r.randrange(2) for _ in rb[0]])
                for _ in range(3):
                    yy=r.randrange(len(rb)); xx=r.randrange(len(rb[0])); rb[yy][xx]^=1
            bb=rb if rb is not None else b
            x=r.randrange(0,w-len(bb[0])); y=r.randrange(0,h-len(bb)); 
            insts.append((sid,x,y,rb) if rb is not None else (sid,x,y))
            place(img,bb,x,y)
        sd=symdict_seg(1,[],syms,None,tpl)
        ts=text_seg(2,w,h,0,0,insts,syms,[1],strips_log,corner,transposed,refine=(0 if refine else None),rtpl=rtpl)
        if use_globals:
            return dict(w=w,h=h,img=img,stream=page_stream(w,h,[ts]),globals=sd)
        return dict(w=w,h=h,img=img,stream=page_stream(w,h,[sd,ts]),globals=None)
mk_text('text_basic')
mk_text('text_tpl1',tpl=1,seed=2)
mk_text('text_tpl2',tpl=2,seed=3)
mk_text('text_tpl3',tpl=3,seed=4)
mk_text('text_globals',use_globals=True,seed=5)
mk_text('text_strips4',strips_log=2,seed=6)
for c in range(4):
    mk_text(f'text_corner{c}',corner=c,seed=10+c)
    mk_text(f'text_corner{c}_tr',corner=c,transposed=True,seed=20+c)
mk_text('text_refine',refine=True,seed=30)
mk_text('text_refine_t1',refine=True,rtpl=1,seed=31)

@case('halftone')
def _():
    w,h=96,80; pw=ph=4
    r=random.Random(5)
    pats=[]
    for g in range(9):
        p=blank(pw,ph); cells=[(i,j) for j in range(ph) for i in range(pw)]; r.shuffle(cells)
        for (i,j) in cells[:min(g*2,16)]: p[j][i]=1
        pats.append(p)
    gw,gh=24,20
    grid=[[r.randrange(len(pats)) for _ in range(gw)] for _ in range(gh)]
    img=blank(w,h)
    for m in range(gh):
        for n in range(gw):
            place(img,pats[grid[m][n]],n*pw,m*ph,0)
    pd=pattern_seg(1,pats,pw,ph,0)
    hs=halftone_seg(2,1,pats,grid,w,h,0,0,0,0,pw*256,0,0)
    # patterns in global (page 0)
    return dict(w=w,h=h,img=img,stream=page_stream(w,h,[pd,hs]),globals=None)

@case('halftone_skip_rot')
def _():
    w,h=90,70; pw=ph=3
    r=random.Random(8)
    pats=[]
    for g in range(5):
        p=blank(pw,ph); cells=[(i,j) for j in range(ph) for i in range(pw)]; r.shuffle(cells)
        for (i,j) in cells[:g*2]: p[j][i]=1
        pats.append(p)
    gw,gh=40,30
    grid=[[r.randrange(5) for _ in range(gw)] for _ in range(gh)]
    gx,gy,rx,ry=-3*256,10*256,3*256,1*256
    img=blank(w,h)
    for m in range(gh):
        for n in range(gw):
            xx=(gx+m*ry+n*rx)>>8; yy=(gy+m*rx-n*ry)>>8
            place(img,pats[grid[m][n]],xx,yy,0)
    # skipped cells keep value 0 -> pattern 0 (all white); the encoder zeroes them
    pd=pattern_seg(1,pats,pw,ph,1)
    hs=halftone_seg(2,1,pats,grid,w,h,0,0,gx,gy,rx,ry,2,0,0,True)
    # decoder uses gray=0 for skipped cells, so reproduce that in the expectation
    img2=blank(w,h)
    for m in range(gh):
        for n in range(gw):
            xx=(gx+m*ry+n*rx)>>8; yy=(gy+m*rx-n*ry)>>8
            skip = xx+pw<=0 or xx>=w or yy+ph<=0 or yy>=h
            g=0 if skip else grid[m][n]
            place(img2,pats[g],xx,yy,0)
    return dict(w=w,h=h,img=img2,stream=page_stream(w,h,[pd,hs]),globals=None)

# ---------------- Huffman, MMR, refinement regions
from jb2huff import *

def mk_text_huff(name,seed=1,strips_log=0,corner=1,mmr=False,lens=None,globals_=False,refine=False,rtpl=0,tabs=None,dht=4,dwt=2,custom=False):
    @case(name)
    def _():
        syms=glyphs(12,seed); w,h=120,90
        r=random.Random(seed); insts=[]; img=blank(w,h)
        for k in range(40):
            sid=r.randrange(len(syms)); b=syms[sid]; rb=None
            if refine and r.random()<0.4:
                rb=[row[:] for row in b]
                for row in rb: row.append(r.randrange(2))
                rb.append([r.randrange(2) for _ in rb[0]])
            bb=rb if rb is not None else b
            x=r.randrange(0,w-len(bb[0])); y=r.randrange(0,h-len(bb))
            insts.append((sid,x,y,rb) if rb is not None else (sid,x,y)); place(img,bb,x,y)
        kw=dict(tabs or {})
        extra=[]
        if custom:
            kw['fs_user']=CUSTOM; extra=[1]
            sd=symdict_huff_seg(2,[],syms,mmr,dht=dht,dwt=dwt); segs_pre=[custom_table_seg(1),sd]; sdn=2; tn=3
        else:
            sd=symdict_huff_seg(1,[],syms,mmr,dht=dht,dwt=dwt); segs_pre=[sd]; sdn=1; tn=2
        ts=text_huff_seg(tn,w,h,0,0,insts,syms,[sdn],strips_log,corner,lens=lens,refine=refine,rtpl=rtpl,extra_refs=extra,**kw)
        if globals_: return dict(w=w,h=h,img=img,stream=page_stream(w,h,[ts]),globals=sd)
        return dict(w=w,h=h,img=img,stream=page_stream(w,h,segs_pre+[ts]),globals=None)
mk_text_huff('thuff_basic')
mk_text_huff('thuff_mmr_dict',seed=2,mmr=True)
mk_text_huff('thuff_strips',seed=3,strips_log=2)
for c in range(4): mk_text_huff(f'thuff_corner{c}',seed=4+c,corner=c)
mk_text_huff('thuff_varlen',seed=9,lens=[2,3,3,4,4,4,5,5,5,5,5,5])
mk_text_huff('thuff_refine',seed=10,refine=True)
mk_text_huff('thuff_refine_t1',seed=11,refine=True,rtpl=1)
mk_text_huff('thuff_globals',seed=12,globals_=True)
mk_text_huff('thuff_tabs_b7_b9_b12',seed=13,tabs=dict(fst=7,dst=9,dtt=12),dht=5,dwt=3)
mk_text_huff('thuff_tabs_b10_b13_b15',seed=14,tabs=dict(dst=10,dtt=13,rdt=15),refine=True,dht=5,dwt=3)
mk_text_huff('thuff_custom_table',seed=15,custom=True,strips_log=1)

@case('generic_mmr')
def _():
    w,h=70,45; b=rnd_bitmap(w,h,4)
    return dict(w=w,h=h,img=b,stream=page_stream(w,h,[generic_mmr_seg(1,b,w,h,0,0)]),globals=None)

@case('halftone_mmr')
def _():
    w,h=64,48; pw=ph=4; r=random.Random(3); pats=[]
    for g in range(8):
        p=blank(pw,ph); cells=[(i,j) for j in range(ph) for i in range(pw)]; r.shuffle(cells)
        for (i,j) in cells[:g*2]: p[j][i]=1
        pats.append(p)
    gw,gh=16,12; grid=[[r.randrange(8) for _ in range(gw)] for _ in range(gh)]
    img=blank(w,h)
    for m in range(gh):
        for n in range(gw): place(img,pats[grid[m][n]],n*pw,m*ph,0)
    return dict(w=w,h=h,img=img,stream=page_stream(w,h,[pattern_mmr_seg(1,pats,pw,ph),halftone_mmr_seg(2,1,pats,grid,w,h,0,0,0,0,pw*256,0)]),globals=None)

def noisy(b,seed,n=15):
    r=random.Random(seed); o=[row[:] for row in b]
    for _ in range(n):
        y=r.randrange(len(o)); x=r.randrange(len(o[0])); o[y][x]^=1
    return o

for tpl in (0,1):
  for tp in (False,True):
    def _mk(tpl=tpl,tp=tp):
        @case(f'refine_page_t{tpl}{"_tp" if tp else ""}')
        def _():
            w,h=60,40; base=rnd_bitmap(w,h,7); new=noisy(base,8)
            segs=[generic_seg(1,base,w,h,0,0,0)]
            # refine the whole page region: reference is the page itself
            segs.append(refine_seg(2,new,base,w,h,0,0,tpl,tp,typ=42,op=4))
            return dict(w=w,h=h,img=new,stream=page_stream(w,h,segs),globals=None)
    _mk()

@case('refine_intermediate')
def _():
    w,h=50,30; base=rnd_bitmap(w,h,12); new=noisy(base,13)
    segs=[generic_seg(1,base,w,h,0,0,0,typ=36),refine_seg(2,new,base,w,h,0,0,0,False,refs=(1,),typ=42)]
    return dict(w=w,h=h,img=new,stream=page_stream(w,h,segs),globals=None)

# ---------------- refinement/aggregate symbol dictionary, striped pages
def symdict_refagg_seg(num,insyms,newsyms,refs_to,tpl=0,rtpl=0,rat=((-1,-1),(-1,-1)),refs=(),page=0):
    """each new symbol is a refinement of insyms[refs_to[i]] (single instance, REFAGGNINST=1)"""
    at=DEF_AT[tpl]
    mq=MQ(); iadh=IA(); iadw=IA(); iaex=IA(); iaai=IA(); iardx=IA(); iardy=IA()
    codelen=ceil_log2(len(insyms)+len(newsyms)); iaid=IAID(codelen)
    gr=newcx(1<<13 if rtpl==0 else 1<<10)
    n=len(newsyms); i=0; hc=0
    while i<n:
        h=len(newsyms[i]); iadh.enc(mq,h-hc); hc=h; sw=0
        while i<n and len(newsyms[i])==h:
            b=newsyms[i]; w=len(b[0]); iadw.enc(mq,w-sw); sw=w
            iaai.enc(mq,1); iaid.enc(mq,refs_to[i]); iardx.enc(mq,0); iardy.enc(mq,0)
            enc_refine(mq,gr,b,w,h,insyms[refs_to[i]],0,0,rtpl,False,rat)
            i+=1
        iadw.enc(mq,None)
    total=len(insyms)+n
    flags=[False]*len(insyms)+[True]*n
    idx=0;cur=False
    while idx<total:
        run=0
        while idx+run<total and flags[idx+run]==cur: run+=1
        iaex.enc(mq,run); idx+=run; cur=not cur
    f=2|(tpl<<10)|(rtpl<<12)
    d=struct.pack('>H',f)+at_bytes(at if tpl==0 else at[:1])+(at_bytes(rat) if rtpl==0 else b'')+struct.pack('>II',n,n)+mq.flush()
    return seg(num,0,list(refs),page,d)

for rtpl in (0,1):
    def _mk(rtpl=rtpl):
        @case(f'sd_refagg_t{rtpl}')
        def _():
            base=glyphs(6,40+rtpl); w,h=100,60
            # refined variants, sorted by height
            var=[]; src=[]
            r=random.Random(5)
            for k in range(6):
                s=r.randrange(len(base)); b=noisy(base[s],50+k,3); var.append((len(b),k,b,s))
            var.sort(key=lambda t:(t[0],t[1]))
            newsyms=[t[2] for t in var]; refs_to=[t[3] for t in var]
            sd1=symdict_seg(1,[],base,None,0)
            sd2=symdict_refagg_seg(2,base,newsyms,refs_to,0,rtpl,refs=(1,))
            # text region draws only the refined symbols (sd2 exports them; refs: 2 only)
            img=blank(w,h); insts=[]
            for k in range(20):
                sid=r.randrange(len(newsyms)); b=newsyms[sid]
                x=r.randrange(0,w-len(b[0])); y=r.randrange(0,h-len(b)); insts.append((sid,x,y)); place(img,b,x,y)
            ts=text_seg(3,w,h,0,0,insts,newsyms,[2])
            return dict(w=w,h=h,img=img,stream=page_stream(w,h,[sd1,sd2,ts]),globals=None)
    _mk()

@case('striped_unknown_height')
def _():
    w=64; b1=rnd_bitmap(w,20,1); b2=rnd_bitmap(w,20,2); h=40
    img=b1+b2
    segs=[seg(0,48,[],1,page_info(w,0xFFFFFFFF,0,0x8000|20)),
          generic_seg(1,b1,w,20,0,0,0),seg(2,50,[],1,struct.pack('>I',19)),
          generic_seg(3,b2,w,20,0,20,0),seg(4,50,[],1,struct.pack('>I',39)),seg(99,49,[],1,b'')]
    return dict(w=w,h=h,img=img,stream=b''.join(segs),globals=None)

@case('sd_aggregate')
def _():
    base=glyphs(6,77); w,h=100,60; r=random.Random(9)
    aggs=[]
    for k in range(4):
        a,b=r.randrange(len(base)),r.randrange(len(base))
        A=base[a]; B=base[b]; B2=noisy(B,100+k,2)
        wa,wb=len(A[0]),len(B[0]); H=max(len(A),len(B))
        bm=blank(wa+wb,H); place(bm,A,0,0); place(bm,B2,wa,0)
        aggs.append((H,k,bm,a,b,B2))
    aggs.sort(key=lambda t:(t[0],t[1]))
    newsyms=[t[2] for t in aggs]
    at=DEF_AT[0]; rat=((-1,-1),(-1,-1)); rtpl=0
    mq=MQ(); iadh=IA(); iadw=IA(); iaex=IA(); iaai=IA()
    iadt=IA();iafs=IA();iads=IA();iari=IA();iardw=IA();iardh=IA();iardx=IA();iardy=IA()
    total=len(base)+len(newsyms); iaid=IAID(ceil_log2(total)); gr=newcx(1<<13)
    i=0; hc=0; n=len(aggs)
    while i<n:
        H=aggs[i][0]; iadh.enc(mq,H-hc); hc=H; sw=0
        while i<n and aggs[i][0]==H:
            _,_,bm,a,b,B2=aggs[i]; W=len(bm[0]); iadw.enc(mq,W-sw); sw=W
            iaai.enc(mq,2)
            iadt.enc(mq,0); iadt.enc(mq,0)
            iafs.enc(mq,0); iaid.enc(mq,a); iari.enc(mq,0)
            wa=len(base[a][0])
            iads.enc(mq,1); iaid.enc(mq,b); iari.enc(mq,1)
            iardw.enc(mq,0); iardh.enc(mq,0); iardx.enc(mq,0); iardy.enc(mq,0)
            enc_refine(mq,gr,B2,len(B2[0]),len(B2),base[b],0,0,rtpl,False,rat)
            iads.enc(mq,None)
            i+=1
        iadw.enc(mq,None)
    flags=[False]*len(base)+[True]*n
    idx=0;cur=False
    while idx<total:
        run=0
        while idx+run<total and flags[idx+run]==cur: run+=1
        iaex.enc(mq,run); idx+=run; cur=not cur
    f=2
    d=struct.pack('>H',f)+at_bytes(at)+at_bytes(rat)+struct.pack('>II',n,n)+mq.flush()
    sd1=symdict_seg(1,[],base,None,0); sd2=seg(2,0,[1],0,d)
    img=blank(w,h); insts=[]
    for k in range(15):
        sid=r.randrange(n); bmp=newsyms[sid]
        x=r.randrange(0,w-len(bmp[0])); y=r.randrange(0,h-len(bmp)); insts.append((sid,x,y)); place(img,bmp,x,y)
    ts=text_seg(3,w,h,0,0,insts,newsyms,[2])
    return dict(w=w,h=h,img=img,stream=page_stream(w,h,[sd1,sd2,ts]),globals=None)
