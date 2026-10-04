#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
inc="$root/crates/w2r-abi/include"
src="$root/crates/w2r-abi/tests/abi_layout.c"
gcc -std=c11 -Wall -Wextra -Wpedantic -Werror -I"$inc" -fsyntax-only "$src"
echo "C11 ok"
g++ -std=c++17 -Wall -Wextra -Werror -I"$inc" -x c++ -fsyntax-only "$src"
echo "C++17 ok"

# 32-bit: gcc -m32 where available (Linux CI), else MSVC x86 via vswhere (Windows dev box).
vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
if echo 'int main(void){return 0;}' | gcc -m32 -x c - -o /dev/null 2>/dev/null; then
  gcc -m32 -std=c11 -Wall -Wextra -Wpedantic -Werror -I"$inc" -fsyntax-only "$src"
  echo "32-bit ok (gcc)"
elif [ -x "$vswhere" ]; then
  vs="$("$vswhere" -latest -products '*' \
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
        -property installationPath | tr -d '\r')"
  [ -n "$vs" ] || { echo "vswhere found no VC tools"; exit 1; }
  bat="$(mktemp --suffix=.bat)"
  trap 'rm -f "$bat"' EXIT
  # /Zs = syntax check only. The W2R_CALL cdecl attribute is only meaningful on x86, which is
  # exactly what vcvars32 selects.
  printf '@echo off\r\ncall "%s" >nul 2>&1\r\ncl /nologo /std:c11 /W4 /WX /Zs /I"%s" "%s"\r\n' \
    "$vs\\VC\\Auxiliary\\Build\\vcvars32.bat" "$(cygpath -w "$inc")" "$(cygpath -w "$src")" > "$bat"
  cmd //c "$(cygpath -w "$bat")" | grep -v '^abi_layout.c$' || true
  test "${PIPESTATUS[0]}" -eq 0
  echo "32-bit ok (msvc)"
else
  echo "32-bit toolchain not available: skipped (CI on Windows covers it)"
fi
