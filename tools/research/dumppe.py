"""Read-only: copy decrypted section bytes of loaded modules out of a running process
into copies of the on-disk PE files. Usage: dumppe.py <process exe name> <outdir> <module1> [module2...]"""
import ctypes, ctypes.wintypes as W, struct, sys, os
k32 = ctypes.WinDLL('kernel32', use_last_error=True)
psapi = ctypes.WinDLL('psapi', use_last_error=True)
k32.OpenProcess.restype = W.HANDLE
k32.ReadProcessMemory.argtypes = [W.HANDLE, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
psapi.EnumProcessModulesEx.argtypes = [W.HANDLE, ctypes.POINTER(W.HMODULE), W.DWORD, ctypes.POINTER(W.DWORD), W.DWORD]
psapi.GetModuleFileNameExW.argtypes = [W.HANDLE, W.HMODULE, W.LPWSTR, W.DWORD]

def find_pid(name):
    import subprocess
    out = subprocess.run(['tasklist','/FO','CSV','/NH','/FI',f'IMAGENAME eq {name}'],capture_output=True,text=True).stdout
    for line in out.splitlines():
        p = [x.strip('"') for x in line.split('","')]
        if len(p) > 1 and p[0].lower() == name.lower(): return int(p[1])
    raise SystemExit('process not found')

def read(h, addr, n):
    buf = ctypes.create_string_buffer(n); got = ctypes.c_size_t()
    ok = k32.ReadProcessMemory(h, ctypes.c_void_p(addr), buf, n, ctypes.byref(got))
    return buf.raw[:got.value], bool(ok)

pid = find_pid(sys.argv[1]); outdir = sys.argv[2]; want = [m.lower() for m in sys.argv[3:]]
os.makedirs(outdir, exist_ok=True)
h = k32.OpenProcess(0x0410, False, pid)  # QUERY_INFORMATION|VM_READ
if not h: raise SystemExit(f'OpenProcess failed {ctypes.get_last_error()}')
mods = (W.HMODULE*2048)(); need = W.DWORD()
psapi.EnumProcessModulesEx(h, mods, ctypes.sizeof(mods), ctypes.byref(need), 3)
for i in range(need.value//ctypes.sizeof(W.HMODULE)):
    path = ctypes.create_unicode_buffer(1024)
    psapi.GetModuleFileNameExW(h, mods[i], path, 1024)
    base = mods[i]; bn = os.path.basename(path.value)
    if bn.lower() not in want: continue
    disk = bytearray(open(path.value,'rb').read())
    pe = struct.unpack_from('<I', disk, 0x3c)[0]
    ns, = struct.unpack_from('<H', disk, pe+6); so, = struct.unpack_from('<H', disk, pe+20)
    img, = struct.unpack_from('<Q', disk, pe+24+24)
    print(f'{bn}: loaded at {base:#x}, disk ImageBase {img:#x}')
    changed = 0
    for s in range(ns):
        o = pe+24+so+s*40
        name = disk[o:o+8].rstrip(b'\0').decode(); vs, va, rs, rp = struct.unpack_from('<IIII', disk, o+8)
        fl, = struct.unpack_from('<I', disk, o+36)
        if not rs: continue
        n = min(vs, rs)
        mem, ok = read(h, base+va, n)
        if len(mem) != n: print(f'  {name}: short read {len(mem)}/{n}'); continue
        # only take executable sections (code); data would be relocated/runtime-mutated
        if fl & 0x20000000:
            disk[rp:rp+n] = mem; changed += 1
            zeros = mem.count(0)
            print(f'  {name}: copied {n:#x} bytes (zero bytes {zeros})')
    open(os.path.join(outdir, bn), 'wb').write(disk)
    print('  wrote', os.path.join(outdir, bn))
