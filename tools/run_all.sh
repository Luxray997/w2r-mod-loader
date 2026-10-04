#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
py=${PYTHON:-python3}
"$py" -m unittest discover -s tools/tests -t tools
bash tools/check_abi.sh
