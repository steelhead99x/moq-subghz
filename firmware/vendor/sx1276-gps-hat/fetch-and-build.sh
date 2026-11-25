#!/usr/bin/env bash
# Fetch the HAT maker's official ARM single-board computer sample app at a pinned commit, check its hash,
# apply the four-constant bench patch, and build it on a ARM single-board computer. Needs git, g++ and WiringPi.
#   ./fetch-and-build.sh [workdir]        default workdir: ./build
# Nothing from the vendor's repository is stored in this repository: it has no license file.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
W=${1:-$HERE/build}
COMMIT=f6e1a55f14c04b16d1a376160e9ccf6891a648e1
UPSTREAM_SHA=355bbdb97e514742af81cec1546bf3fc3ec74f45de15d5b2930bb96741c3c73b   # dragino_lora_app/main.c
PATCHED_SHA=0e179180ae3d48dbeb49198ffe84e329bef4e7ed0d1ed88442ba04e73d8b892c    # same file after the patch
mkdir -p "$W" && cd "$W"
[ -d rpi-lora-tranceiver ] || git clone https://github.com/dragino/rpi-lora-tranceiver.git
git -C rpi-lora-tranceiver checkout -q "$COMMIT"
cd rpi-lora-tranceiver/dragino_lora_app
echo "$UPSTREAM_SHA  main.c" | sha256sum -c -
git checkout -q -- main.c
patch --no-backup-if-mismatch main.c < "$HERE/gps-hat-bench.patch"
echo "$PATCHED_SHA  main.c" | sha256sum -c -
# WiringPi: Debian-based OS Trixie has no package. Either install it, or build v3.20 and point at it:
#   git clone --branch v3.20 https://github.com/WiringPi/WiringPi && make -C WiringPi/wiringPi
#   ln -sf libwiringPi.so.3.20 WiringPi/wiringPi/libwiringPi.so ; export WIRINGPI=$PWD/WiringPi/wiringPi
if [ -n "${WIRINGPI:-}" ]; then
  g++ -Wall -I"$WIRINGPI" -c main.c -o main.o
  g++ main.o -L"$WIRINGPI" -lwiringPi -Wl,-rpath,"$WIRINGPI" -o hat_app
else
  g++ -Wall -c main.c -o main.o && g++ main.o -lwiringPi -o hat_app
fi
echo "built $PWD/hat_app"
