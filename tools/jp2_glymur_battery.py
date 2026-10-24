"""Checks crates/oc-jp2 against OpenJPEG (through glymur) on a battery of encoder settings.

Build the example first (cargo build --release -p oc-jp2 --examples), then:
    python3 tools/jp2_glymur_battery.py SCRATCH_DIR [case ...]
Needs glymur, numpy and a libopenjp2.
"""
import sys, subprocess, numpy as np, glymur, warnings
warnings.simplefilter("ignore")
S=sys.argv[1]; DEC="target/release/examples/dec"
rng=np.random.default_rng(5)
def img(h,w,c,bits=8):
    y,x=np.mgrid[0:h,0:w]
    base=[(np.sin(x/7.0+k)*0.5+0.5)*(np.cos(y/9.0)*0.5+0.5) for k in range(max(c,1))]
    a=np.stack(base,-1)*(2**bits-1)+rng.normal(0,6,(h,w,max(c,1)))
    a=np.clip(a,0,2**bits-1).astype(np.uint8 if bits<=8 else np.uint16)
    return a if c>0 else a[...,0]
cases={
 'bypass': dict(modesw=1),
 'reset': dict(modesw=2),
 'termall': dict(modesw=4),
 'vsc': dict(modesw=8),
 'pterm': dict(modesw=16),
 'segsym': dict(modesw=32),
 'all': dict(modesw=63),
 'bypass_termall': dict(modesw=5),
 'prec': dict(psizes=[(32,32),(32,32),(64,64),(64,64)], numres=4, cbsize=(16,16)),
 'prec_rpcl': dict(psizes=[(16,16),(32,32),(64,64)], numres=3, prog='RPCL', cbsize=(8,8)),
 'prec_pcrl': dict(psizes=[(32,32),(32,32),(64,64)], numres=3, prog='PCRL', cbsize=(16,16)),
 'prec_cprl': dict(psizes=[(32,32),(32,32),(64,64)], numres=3, prog='CPRL', tilesize=(120,90), cbsize=(16,16)),
 'irrev': dict(irreversible=True),
 'irrev_layers': dict(irreversible=True, cratios=[30,10,4]),
 'cb16': dict(cbsize=(16,16)),
 'cb64x16': dict(cbsize=(64,16)),
 'tiles': dict(tilesize=(64,64)),
 'layers': dict(cratios=[40,20,10,1]),
 'layers_rlcp': dict(cratios=[40,20,10,1], prog='RLCP'),
 'sop_eph': dict(sop=True, eph=True),
 'plt': dict(plt=True),
}
w,h=int(sys.argv[2]),int(sys.argv[3])
only=sys.argv[4:] 
for name,kw in cases.items():
    if only and name not in only: continue
    a=img(h,w,3)
    f=f"{S}/g_{name}.jp2"
    try:
        glymur.Jp2k(f, data=a, **kw)
    except Exception as e:
        print("ENCFAIL",name,e); continue
    ref=glymur.Jp2k(f)[:]
    out=subprocess.run([DEC,f],capture_output=True)
    got=np.frombuffer(out.stdout,dtype=np.uint8)
    ok = got.size==ref.size and (got==ref.reshape(-1)).all()
    if not ok and got.size==ref.size:
        d=np.abs(got.astype(int)-ref.reshape(-1).astype(int)); print("FAIL",name,"maxdiff",d.max(),"n",(d>0).sum(), out.stderr.decode()[:100])
    else:
        print("PASS" if ok else "FAIL",name, out.stderr.decode()[:100] if not ok else "")
