#!/usr/bin/env bash
# Benchmarks parch against zip/unzip (and ditto on macOS). Run from the repository root:
#   [LEVEL=N] scripts/bench.sh [FILES]   (default 100000 small files; LEVEL 0-9 for zip and parch, default 6)
# Data lives in target/bench. Times are wall clock; CPU = (user+sys)/real.
set -uo pipefail

FILES=${1:-100000}
LEVEL=${LEVEL:-6}
DIR=target/bench
PARCH=$PWD/target/release/parch
cargo build --release -q || exit 2
mkdir -p "$DIR"
cd "$DIR"

[ -d many ] || python3 -I ../../scripts/gen_tree.py many --files "$FILES" --dirs 1000
[ -f big/big.bin ] || python3 -I ../../scripts/gen_tree.py big --files 0 --dirs 1 --big-mb 2048
[ -d src ] || (cd ../.. && cargo vendor -q target/bench/src >/dev/null)

# Stand-in for GNU `time -v` when the `time` package is missing: prints the fields run() reads.
GNU_TIME='import resource, subprocess, sys, time
t = time.monotonic(); code = subprocess.call(sys.argv[1:]); t = time.monotonic() - t
r = resource.getrusage(resource.RUSAGE_CHILDREN)
print(f"Elapsed (wall clock) time (h:mm:ss or m:ss): {int(t // 60)}:{t % 60:05.2f}", f"User time (seconds): {r.ru_utime:.2f}",
      f"System time (seconds): {r.ru_stime:.2f}", f"Maximum resident set size (kbytes): {r.ru_maxrss}", sep="\n", file=sys.stderr)
sys.exit(code)'
if [ "$(uname)" = Darwin ]; then TIME=(/usr/bin/time -l); RSS='maximum resident set size'
elif [ -x /usr/bin/time ]; then TIME=(/usr/bin/time -v); RSS='Maximum resident set size'
else TIME=(python3 -I -c "$GNU_TIME"); RSS='Maximum resident set size'; fi

run() { # label output-file command...
  local label=$1 out=$2; shift 2
  rm -rf "$out"
  local log
  log=$({ "${TIME[@]}" "$@" >/dev/null; } 2>&1)
  local real user sys rss size
  if [ "$(uname)" = Darwin ]; then
    read -r real user sys < <(echo "$log" | awk '/ real /{print $1, $3, $5}')
    rss=$(echo "$log" | awk -v k="$RSS" 'index($0,k){print int($1/1048576)}')
  else
    real=$(echo "$log" | awk -F': ' '/Elapsed/{split($2,a,":"); print a[1]*60+a[2]}')
    user=$(echo "$log" | awk -F': ' '/User time/{print $2}')
    sys=$(echo "$log" | awk -F': ' '/System time/{print $2}')
    rss=$(echo "$log" | awk -F': ' -v k="$RSS" 'index($0,k){print int($2/1024)}')
  fi
  size=$( [ -f "$out" ] && wc -c < "$out" | tr -d ' ' || echo -)
  printf '%-28s %8.2fs  CPU %5.1f  RSS %5s MiB  size %s\n' "$label" "$real" "$(echo "($user+$sys)/$real" | bc -l)" "$rss" "$size"
}

echo "level $LEVEL"
for set in many big src; do
  tar -cf - "$set" | cat > /dev/null   # warm the page cache (GNU tar skips reading files for /dev/null)
  echo "== create: $set"
  run "parch c" p.zip "$PARCH" c -q -l "$LEVEL" p.zip "$set"
  command -v zip >/dev/null && run "zip -${LEVEL}r" z.zip zip -q -"$LEVEL"r z.zip "$set"
  command -v ditto >/dev/null && run "ditto -c -k" d.zip ditto -c -k --norsrc "$set" d.zip
  echo "== extract/test: $set"
  run "parch x" xp "$PARCH" x -q p.zip -C xp
  command -v unzip >/dev/null && run "unzip" xu unzip -q p.zip -d xu
  run "parch t" none "$PARCH" t -q p.zip
  command -v unzip >/dev/null && run "unzip -t" none unzip -tq p.zip
  rm -rf xp xu p.zip z.zip d.zip
done
