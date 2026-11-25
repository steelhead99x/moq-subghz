#!/usr/bin/env bash
# Build the Linux release archives and their SHA256SUMS into dist/.
#
#   scripts/release.sh VERSION            e.g. scripts/release.sh v0.1.1
#
# What it does, and why:
# * Both Linux targets are built against musl, statically linked, so the binaries start on any
#   Linux of that architecture whatever its glibc. (v0.1.0's x86-64 binaries were glibc builds that
#   needed GLIBC_2.39.) moq-subghz-publish has its own toolchain file and is built the same way.
# * Each archive carries the reference inputs the benchmark tool reads (results/css-2015/frames,
#   results/factory-firmware, the source picture under media/) beside bin/. moq-subghz-bench finds
#   them relative to its own path, so an unpacked archive needs no source checkout.
# * Every binary is checked: a dynamically linked one has its highest GLIBC_ symbol version written
#   to REQUIREMENTS.txt in the archive, so a libc requirement is never left unstated.
# * The host archive is unpacked in a scratch directory and `moq-subghz-bench css2015-table` is run
#   from another working directory, which proves the packaged inputs resolve.
#
# Needs: rustup with the targets below, tar, sha256sum, readelf/objdump (binutils).
set -euo pipefail

VERSION=${1:?usage: scripts/release.sh VERSION}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST="$ROOT/dist"
cd "$ROOT"

WORKSPACE_PKGS=(-p moq-subghz-send -p moq-subghz-recv -p moq-subghz-css2015 -p moq-subghz-bench
                -p moq-subghz-report -p moq-subghz-radio-emu)
# target triple | archive label | binaries | build publish too
TARGETS=(
  "x86_64-unknown-linux-musl|linux-x86_64|moq-subghz-send moq-subghz-recv moq-subghz-bench moq-subghz-report moq-subghz-chan css2015-send css2015-log moq-subghz-publish|yes"
  "armv7-unknown-linux-musleabihf|linux-armv7-musleabihf|moq-subghz-send moq-subghz-recv moq-subghz-bench css2015-send css2015-log|no"
)
# The inputs moq-subghz-bench reads, relative to the repository root.
INPUTS=(results/css-2015/frames results/factory-firmware/results.csv results/factory-firmware/data
        results/factory-firmware/README.md media/frame-f350-rind-120s.png)

# Highest GLIBC_x.y a binary needs, or empty when it is static or not linked against glibc.
glibc_need() {
  if ! readelf -d "$1" 2>/dev/null | grep -q NEEDED; then return 0; fi
  objdump -T "$1" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -n 1
}

rm -rf "$DIST"
mkdir -p "$DIST"
for t in "${TARGETS[@]}"; do
  IFS='|' read -r triple label bins publish <<<"$t"
  rustup target add "$triple" >/dev/null
  cargo build --release --locked --target "$triple" "${WORKSPACE_PKGS[@]}"
  if [[ $publish == yes ]]; then
    (cd moq-subghz-publish && rustup target add "$triple" >/dev/null &&
       cargo build --release --locked --target "$triple")
  fi
  name="moq-subghz-$VERSION-$label"
  stage="$DIST/$name"
  mkdir -p "$stage/bin"
  for b in $bins; do
    src="target/$triple/release/$b"
    [[ $b == moq-subghz-publish ]] && src="moq-subghz-publish/target/$triple/release/$b"
    install -m 0755 "$src" "$stage/bin/$b"
  done
  cp README.md LICENSE-APACHE LICENSE-MIT "$stage/"
  for i in "${INPUTS[@]}"; do
    mkdir -p "$stage/$(dirname "$i")"
    cp -R "$i" "$stage/$i"
  done
  {
    echo "moq-subghz $VERSION, $label"
    echo
    echo "Runtime requirements:"
    any=no
    for b in "$stage"/bin/*; do
      need=$(glibc_need "$b")
      if [[ -n $need ]]; then
        echo "  $(basename "$b"): glibc ${need#GLIBC_} or newer"
        any=yes
      fi
    done
    [[ $any == no ]] && echo "  none: every binary is statically linked (musl) and runs on any Linux of this architecture"
    echo "  FFmpeg (ffmpeg, ffplay) is installed separately for decoding and playback."
    echo
    echo "moq-subghz-bench reads its reference inputs from results/ and media/ in this directory,"
    echo "found relative to bin/. Keep them beside bin/, or pass --repo-root (or MOQ_SUBGHZ_ROOT)"
    echo "to a checkout of the repository."
  } >"$stage/REQUIREMENTS.txt"
  tar -C "$DIST" --owner=0 --group=0 -czf "$DIST/$name.tar.gz" "$name"
done

# Smoke test the host archive as a user would get it: unpacked elsewhere, run from elsewhere.
host_label="linux-$(uname -m)"
if [[ -f "$DIST/moq-subghz-$VERSION-$host_label.tar.gz" ]]; then
  smoke=$(mktemp -d)
  trap 'rm -rf "$smoke"' EXIT
  tar -C "$smoke" -xzf "$DIST/moq-subghz-$VERSION-$host_label.tar.gz"
  (cd / && env -u MOQ_SUBGHZ_ROOT "$smoke/moq-subghz-$VERSION-$host_label/bin/moq-subghz-bench" css2015-table >/dev/null)
  echo "smoke test: css2015-table ran from the unpacked $host_label archive"
fi

cp firmware/rp2040-sx1276-fw-v5.uf2 "$DIST/"
(cd "$DIST" && sha256sum -- *.tar.gz *.uf2 >SHA256SUMS)
for t in "${TARGETS[@]}"; do
  IFS='|' read -r _ label _ _ <<<"$t"
  rm -rf "${DIST:?}/moq-subghz-$VERSION-$label"
done
cat "$DIST/SHA256SUMS"
