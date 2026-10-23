#!/usr/bin/env bash
# Tests and packages parch on a Debian/Ubuntu host (or in the ubuntu-26.04 build image).
# Run from the repository root: scripts/release/build.sh
# Container variant:
#   docker build -t parch-build:ubuntu-26.04 -f scripts/release/ubuntu-26.04.Dockerfile scripts/release
#   docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -e CARGO_HOME=/src/target/cargo-home \
#     -v "$PWD":/src -w /src parch-build:ubuntu-26.04 scripts/release/build.sh
# Produces dist/: tar.gz and .deb for the native architecture (and x86_64 when the cross
# toolchain is installed), plus SHA256SUMS. SKIP_CHECKS=1 skips tests and compatibility checks.
set -euo pipefail
umask 022

JOBS=${JOBS:-4}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
NATIVE=$(rustc -vV | sed -n 's/^host: //p')
CROSS=x86_64-unknown-linux-gnu
DIST=dist
MAINTAINER=${MAINTAINER:-"egen <tbi.eug@gmail.com>"}

export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc
export AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar

if [ "${SKIP_CHECKS:-0}" != 1 ]; then
  echo "== tests ($NATIVE)"
  cargo test --workspace --locked -j "$JOBS"
  echo "== compatibility"
  scripts/compat.sh
fi
echo "== release builds"
cargo build --release --locked -j "$JOBS"
BUILD_CROSS=0
if [ "$NATIVE" != "$CROSS" ] && command -v x86_64-linux-gnu-gcc >/dev/null && rustup target list --installed 2>/dev/null | grep -qx "$CROSS"; then
  BUILD_CROSS=1
  cargo build --release --locked -j "$JOBS" --target "$CROSS"
else
  echo "x86_64 cross toolchain not installed: building $NATIVE only"
fi

deb_arch() {
  case $1 in
    x86_64-*) echo amd64 ;;
    aarch64-*) echo arm64 ;;
    *) echo "unsupported target $1" >&2; exit 1 ;;
  esac
}

package() {
  local triple=$1 bin=$2 tools=$3
  local arch name stage deb glibc depends
  arch=$(deb_arch "$triple")
  name=parch-$VERSION-$triple
  stage=$DIST/stage/$name
  mkdir -p "$stage"
  "${tools}strip" -o "$stage/parch" "$bin"
  tar -C "$DIST/stage" -czf "$DIST/$name.tar.gz" "$name"

  glibc=$("${tools}objdump" -T "$stage/parch" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1)
  depends="libc6 (>= $glibc)"
  if "${tools}objdump" -p "$stage/parch" | grep -q 'NEEDED.*libgcc_s'; then
    depends="$depends, libgcc-s1"
  fi
  deb=$DIST/stage/deb-$arch
  mkdir -p "$deb/DEBIAN" "$deb/usr/bin"
  install -m 0755 "$stage/parch" "$deb/usr/bin/parch"
  cat > "$deb/DEBIAN/control" <<CONTROL
Package: parch
Version: $VERSION-1
Architecture: $arch
Maintainer: $MAINTAINER
Installed-Size: $(du -k "$deb/usr/bin/parch" | cut -f1)
Depends: $depends
Section: utils
Priority: optional
Description: parallel ZIP archiver
 Creates, extracts, lists, tests and updates ZIP archives in parallel,
 using three quarters of the CPU cores by default.
CONTROL
  dpkg-deb --root-owner-group --build "$deb" "$DIST/parch_${VERSION}-1_${arch}.deb" >/dev/null
  echo "$arch: $("${tools}objdump" -f "$stage/parch" | sed -n 's/.*file format //p'); Depends: $depends"
}

echo "== packages"
rm -rf "$DIST" && mkdir -p "$DIST"
package "$NATIVE" target/release/parch ""
if [ "$BUILD_CROSS" = 1 ]; then
  package "$CROSS" "target/$CROSS/release/parch" x86_64-linux-gnu-
fi
rm -rf "$DIST/stage"
(cd "$DIST" && sha256sum ./*.tar.gz ./*.deb > SHA256SUMS)
ls -l "$DIST"
