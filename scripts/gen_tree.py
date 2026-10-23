#!/usr/bin/env python3
"""Deterministic test tree generator: many small text-like files plus optional big files.

usage: gen_tree.py DIR [--files N] [--dirs N] [--big-mb N] [--seed N]
"""
import argparse
import os
import random

WORDS = ("archive parallel deflate stream chunk thread worker budget entry header central "
         "directory local offset crc zip64 extra field unix windows symlink metadata").split()


def text(rng, size):
    out = []
    n = 0
    while n < size:
        w = rng.choice(WORDS)
        out.append(w)
        n += len(w) + 1
        if rng.random() < 0.08:
            out.append("\n")
    return (" ".join(out))[:size].encode()


def main():
    p = argparse.ArgumentParser()
    p.add_argument("dir")
    p.add_argument("--files", type=int, default=100_000)
    p.add_argument("--dirs", type=int, default=1_000)
    p.add_argument("--big-mb", type=int, default=0)
    p.add_argument("--seed", type=int, default=1)
    a = p.parse_args()
    rng = random.Random(a.seed)
    dirs = [os.path.join(a.dir, f"d{i // 100:03d}", f"s{i % 100:02d}") for i in range(a.dirs)]
    for d in dirs:
        os.makedirs(d, exist_ok=True)
    for i in range(a.files):
        size = int(rng.lognormvariate(8, 1.2)) % 200_000
        with open(os.path.join(rng.choice(dirs), f"f{i:07d}.txt"), "wb") as f:
            f.write(text(rng, size))
    if a.big_mb:
        with open(os.path.join(a.dir, "big.bin"), "wb") as f:
            chunk = text(rng, 1 << 20)
            for _ in range(a.big_mb):
                f.write(chunk)
                f.write(rng.randbytes(4096))


if __name__ == "__main__":
    main()
