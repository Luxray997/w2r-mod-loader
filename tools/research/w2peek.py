import ctypes, ctypes.wintypes as W, struct, subprocess, sys
k32=ctypes.WinDLL('kernel32',use_last_error=True); psapi=ctypes.WinDLL('psapi',use_last_error=True)
k32.OpenProcess.restype=W.HANDLE
k32.ReadProcessMemory.argtypes=[W.HANDLE,ctypes.c_void_p,ctypes.c_void_p,ctypes.c_size_t,ctypes.POINTER(ctypes.c_size_t)]
psapi.EnumProcessModulesEx.argtypes=[W.HANDLE,ctypes.POINTER(W.HMODULE),W.DWORD,ctypes.POINTER(W.DWORD),W.DWORD]
psapi.GetModuleFileNameExW.argtypes=[W.HANDLE,W.HMODULE,W.LPWSTR,W.DWORD]
out=subprocess.run(['tasklist','/FO','CSV','/NH','/FI','IMAGENAME eq Warcraft II.exe'],capture_output=True,text=True).stdout
pid=int(out.split('","')[1])
h=k32.OpenProcess(0x0410,False,pid)
mods=(W.HMODULE*1024)(); need=W.DWORD(); psapi.EnumProcessModulesEx(h,mods,ctypes.sizeof(mods),ctypes.byref(need),3)
import os
base={}
for i in range(need.value//ctypes.sizeof(W.HMODULE)):
    p=ctypes.create_unicode_buffer(512); psapi.GetModuleFileNameExW(h,mods[i],p,512); base[os.path.basename(p.value).lower()]=mods[i]
print('exe base',hex(base['warcraft ii.exe']),'sdk base',hex(base.get('clientsdk.dll',0)))
def rd(a,n):
    b=ctypes.create_string_buffer(n); g=ctypes.c_size_t(); k32.ReadProcessMemory(h,ctypes.c_void_p(a),b,n,ctypes.byref(g)); return b.raw[:g.value]
def u32(a):
    r=rd(a,4); return struct.unpack('<I',r)[0] if len(r)==4 else None
eb=base['warcraft ii.exe']; image=0x400000
cont=eb+(0x964d78-image)
print('container',hex(cont))
# service wrappers +4 -> sdk object; print each service's wrapper vtable and sdk obj/vtable
for off in range(0x50,0x130,4):
    w=u32(cont+off)
    if w and 0x10000<w<0x7fff0000: 
        pass
for name,off in [('gateway',0xb8),('localws',0xf8)]:
    wrap=cont+off; vt=u32(wrap); sdk=u32(wrap+4)
    print(name,'wrapper',hex(wrap),'vt',hex(vt or 0),'sdkobj',hex(sdk or 0))
    if sdk:
        svt=u32(sdk); print('  sdk vtable',hex(svt), 'rva in sdk', hex(svt-base['clientsdk.dll']))
        print('  object dwords:',' '.join(hex(u32(sdk+4*i) or 0) for i in range(0,40)))
        print('  slots:',' '.join(hex(u32(svt+4*i)-base['clientsdk.dll']+0x10000000) for i in range(0,18)))
