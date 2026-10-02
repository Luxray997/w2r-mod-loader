#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
inc="$root/crates/w2r-abi/include"
src="$root/crates/w2r-abi/tests/abi_layout.c"
gcc -std=c11 -Wall -Wextra -Wpedantic -Werror -I"$inc" -fsyntax-only "$src"
echo "C11 ok"
g++ -std=c++17 -Wall -Wextra -Werror -I"$inc" -x c++ -fsyntax-only "$src"
echo "C++17 ok"
if echo 'int main(void){return 0;}' | gcc -m32 -x c - -o /dev/null 2>/dev/null; then
  gcc -m32 -std=c11 -Wall -Wextra -Wpedantic -Werror -I"$inc" -fsyntax-only "$src"
  echo "32-bit ok"
else
  echo "32-bit toolchain not available: skipped (CI on Windows covers it)"
fi
