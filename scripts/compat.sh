#!/usr/bin/env bash
# Cross-tool compatibility check. Run from the repository root: scripts/compat.sh
# Archives made by parch are verified and extracted by every available tool
# (unzip, zipinfo, bsdtar, python3 zipfile, ditto, 7z/7zz); archives made by those tools
# are verified and extracted by parch. Extracted trees are compared with the source.
set -uo pipefail

ROOT=target/compat
PARCH=target/release/parch
cargo build --release -q || exit 2
rm -rf "$ROOT" && mkdir -p "$ROOT"
python3 -I scripts/gen_tree.py "$ROOT/src" --files 2000 --dirs 50 --big-mb 24 --seed 7
mkdir -p "$ROOT/src/empty_dir" && : > "$ROOT/src/empty.txt"
printf 'Привет\n' > "$ROOT/src/юникод.txt"
ln -s big.bin "$ROOT/src/link" 2>/dev/null || true

fail=0
pass() { printf '  %-44s ok\n' "$1"; }
bad() { printf '  %-44s FAILED\n' "$1"; fail=1; }
check() { local name=$1; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else bad "$name"; fi; }
have() { command -v "$1" >/dev/null 2>&1; }
same() { diff -r --no-dereference "$ROOT/src" "$1/src" >/dev/null 2>&1; }

echo "== archives written by parch"
(cd "$ROOT" && ../release/parch c -q default.zip src)
(cd "$ROOT" && ../release/parch c -q -0 store.zip src)
(cd "$ROOT" && ../release/parch c -q --zip64 always zip64.zip src)
(cd "$ROOT" && ../release/parch c -q --big-file 1M - src > stream.zip)
for z in default store zip64 stream; do
  a="$ROOT/$z.zip"
  check "$z: parch t" "$PARCH" t -q "$a"
  have unzip && check "$z: unzip -t" unzip -tq "$a"
  have zipinfo && check "$z: zipinfo" zipinfo "$a"
  have bsdtar && check "$z: bsdtar -tf" bsdtar -tf "$a"
  have python3 && check "$z: python3 zipfile -t" python3 -I -m zipfile -t "$a"
  for t in 7zz 7z; do have $t && { check "$z: $t t" $t t "$a"; break; }; done
  if have unzip; then
    rm -rf "$ROOT/x" && mkdir -p "$ROOT/x" && (cd "$ROOT/x" && unzip -q "../$z.zip") && same "$ROOT/x" && pass "$z: unzip extract == source" || bad "$z: unzip extract == source"
  fi
  if have ditto; then
    rm -rf "$ROOT/x" && ditto -x -k "$a" "$ROOT/x" && same "$ROOT/x" && pass "$z: ditto extract == source" || bad "$z: ditto extract == source"
  fi
  if have bsdtar; then
    rm -rf "$ROOT/x" && mkdir -p "$ROOT/x" && (cd "$ROOT/x" && bsdtar -xf "../$z.zip") && cmp -s "$ROOT/src/big.bin" "$ROOT/x/src/big.bin" && pass "$z: bsdtar extract" || bad "$z: bsdtar extract"
  fi
done

echo "== archives read by parch"
make_and_check() {
  local name=$1 file=$2; shift 2
  if (cd "$ROOT" && "$@") >/dev/null 2>&1; then
    check "$name: parch t" "$PARCH" t -q "$ROOT/$file"
    rm -rf "$ROOT/x" && "$PARCH" x -q "$ROOT/$file" -C "$ROOT/x" && same "$ROOT/x" && pass "$name: parch extract == source" || bad "$name: parch extract == source"
  else
    bad "$name: create"
  fi
}
have zip && make_and_check "zip -ry" infozip.zip zip -qry infozip.zip src
have ditto && make_and_check "ditto -c -k" ditto.zip ditto -c -k --norsrc --keepParent src ditto.zip
have bsdtar && make_and_check "bsdtar --format zip" bsdtar.zip bsdtar --format zip -cf bsdtar.zip src
for t in 7zz 7z; do have $t && { make_and_check "$t a -tzip" 7z.zip $t a -tzip -snl 7z.zip src; break; }; done
if have python3; then
  for m in ZIP_STORED ZIP_DEFLATED ZIP_BZIP2 ZIP_LZMA; do
    (cd "$ROOT" && python3 -I -c "import zipfile,os
with zipfile.ZipFile('py.zip','w',zipfile.$m) as z:
    for d,_,fs in os.walk('src'):
        for f in fs:
            p=os.path.join(d,f)
            if not os.path.islink(p): z.write(p)") >/dev/null 2>&1
    rm -rf "$ROOT/x" && "$PARCH" x -q "$ROOT/py.zip" -C "$ROOT/x" && cmp -s "$ROOT/src/big.bin" "$ROOT/x/src/big.bin" && pass "python3 $m: parch extract" || bad "python3 $m: parch extract"
  done
fi

[ $fail -eq 0 ] && echo "all checks passed" || echo "some checks FAILED"
exit $fail
