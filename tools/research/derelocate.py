"""Undo load-time relocations in a dumped PE32 so absolute addresses match the preferred ImageBase.
Usage: derelocate.py <dump.dll> <loaded_base_hex>"""
import struct,sys
f=sys.argv[1]; loaded=int(sys.argv[2],16)
d=bytearray(open(f,'rb').read())
pe=struct.unpack_from('<I',d,0x3c)[0]; ns,=struct.unpack_from('<H',d,pe+6); so,=struct.unpack_from('<H',d,pe+20)
base,=struct.unpack_from('<I',d,pe+24+28); delta=(loaded-base)&0xffffffff
rrva,rsz=struct.unpack_from('<II',d,pe+24+96+8*5)  # data dir 5 = base reloc
secs=[]
for i in range(ns):
    o=pe+24+so+i*40; nm=d[o:o+8].rstrip(b'\0').decode(); vs,va,rs,rp=struct.unpack_from('<IIII',d,o+8); fl,=struct.unpack_from('<I',d,o+36); secs.append((nm,va,vs,rs,rp,fl))
def rva2off(rva):
    for nm,va,vs,rs,rp,fl in secs:
        if va<=rva<va+max(vs,rs): return rp+rva-va, fl
    return None,0
ro,_=rva2off(rrva); p=ro; end=ro+rsz; fixed=0
while p<end:
    page,blk=struct.unpack_from('<II',d,p)
    if blk<8: break
    for i in range((blk-8)//2):
        e,=struct.unpack_from('<H',d,p+8+i*2)
        if e>>12==3:
            off,fl=rva2off(page+(e&0xfff))
            if off is not None and fl&0x20000000:   # only executable sections were dumped
                v,=struct.unpack_from('<I',d,off); struct.pack_into('<I',d,off,(v-delta)&0xffffffff); fixed+=1
    p+=blk
open(f,'wb').write(d); print('un-relocated',fixed,'entries, delta',hex(delta))
