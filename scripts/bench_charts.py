#!/usr/bin/env python3
"""SVG charts comparing parch with zip and unzip from scripts/bench.sh output.

usage: bench_charts.py [-o DIR] [--name KEY=TEXT]... [--zip LABEL] [--unzip LABEL] LOG [LOG...]

LOG is the output of scripts/bench.sh, or of any run that prints the same result lines after a
"dataset: N files, D dirs, B bytes" line (such a data set is keyed by the log file name).
Writes speedup.svg, times.svg and sizes.svg to DIR (default target/bench/charts). --zip and --unzip
set how the two tools are labeled, for example with their versions.
"""
import argparse
import math
import os
import re
from html import escape

RESULT = re.compile(r"^(?P<label>\S.*?)\s+(?P<secs>\d+(?:\.\d+)?)s\s+CPU\s+\S+\s+RSS\s+(?:\S+\s+)?MiB"
                    r"(?:\s+exit\s+(?P<exit>\d+))?\s+size\s+(?P<size>\S+)\s*$")
SECTION = re.compile(r"^== [a-z/]+: (?P<set>\S+)")
DATASET = re.compile(r"^dataset: (?P<files>\d+) files, \d+ dirs, (?P<bytes>\d+) bytes")
LEVEL = re.compile(r"^level (\d+)$|^zip -(\d+)r$|^zip -r -(\d+)$|^parch c -(\d+)$")
NAMES = {"many": "100,000 small files", "big": "one 2 GiB file", "src": "Rust sources"}
OPS = ("create", "extract", "test")
COLORS = {"zip": "#8c959f", "parch": "#0969da"}
FONT = "-apple-system, 'Segoe UI', Helvetica, Arial, sans-serif"
TEXT, MUTED, GRID = "#1f2328", "#59636e", "#d1d9e0"
WIDTH = 760


def classify(label):
    """Maps a result label to (operation, tool); None for runs that are not charted (other tools, parch -j1)."""
    if label.startswith("parch"):
        if label.startswith("parch x"):
            return "extract", "parch"
        if label.startswith("parch t"):
            return "test", "parch"
        return None if "-j1" in label else ("create", "parch")
    if label.startswith("unzip"):
        return ("test" if "-t" in label else "extract"), "zip"
    if label.startswith("zip"):
        return "create", "zip"
    return None


def parse(paths, names):
    sets, levels = {}, set()
    for path in paths:
        key = None
        stem = os.path.splitext(os.path.basename(path))[0]
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if m := SECTION.match(line):
                    key = m["set"]
                    sets.setdefault(key, {"name": names.get(key, NAMES.get(key, key)), "runs": {}})
                elif m := DATASET.match(line):
                    key = stem
                    name = f"{int(m['files']):,} files, {fmt_size(int(m['bytes']))}"
                    sets.setdefault(key, {"name": names.get(key, name), "runs": {}})
                elif m := LEVEL.match(line):
                    levels.add(int(next(g for g in m.groups() if g)))
                elif (m := RESULT.match(line)) and key and (m["exit"] or "0") == "0":
                    if m2 := LEVEL.match(m["label"]):
                        levels.add(int(next(g for g in m2.groups() if g)))
                    if kind := classify(m["label"]):
                        size = int(m["size"]) if m["size"].isdigit() else None
                        sets[key]["runs"][kind] = (float(m["secs"]), size)
    return [s for s in sets.values() if s["runs"]], levels


def fmt_time(s):
    if s >= 60:
        m, sec = divmod(round(s), 60)
        return f"{m} min {sec} s"
    return f"{s:.2f} s"


def fmt_size(b):
    return f"{b / 2**30:.2f} GiB" if b >= 2**30 else f"{b / 2**20:.1f} MiB"


def text_width(s, size=13):
    return len(s) * size * 0.58


def ticks(top, count=5):
    raw = top / count
    mag = 10 ** math.floor(math.log10(raw))
    step = next(m * mag for m in (1, 2, 2.5, 5, 10) if m * mag >= raw)
    return [i * step for i in range(math.ceil(top / step) + 1)]


class Svg:
    def __init__(self, title, subtitle):
        self.title, self.items, self.y = title, [], 0
        self.text(16, 28, title, size=16, weight="bold")
        self.text(16, 48, subtitle, size=12, color=MUTED)
        self.y = 64

    def text(self, x, y, s, size=13, anchor="start", weight="normal", color=TEXT):
        self.items.append(f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" text-anchor="{anchor}" '
                          f'font-weight="{weight}" fill="{color}">{escape(s)}</text>')

    def rect(self, x, y, w, h, color):
        self.items.append(f'<rect x="{x:.1f}" y="{y:.1f}" width="{max(w, 1):.1f}" height="{h}" rx="2" fill="{color}"/>')

    def line(self, x1, y1, x2, y2, color=GRID, dash=False):
        style = ' stroke-dasharray="4 3"' if dash else ""
        self.items.append(f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" stroke="{color}"{style}/>')

    def legend(self, labels):
        x = 16
        for t, name in labels.items():
            self.rect(x, self.y + 2, 12, 12, COLORS[t])
            self.text(x + 18, self.y + 12, name, size=12)
            x += 18 + text_width(name, 12) + 20
        self.y += 26

    def save(self, path):
        height = self.y + 12
        head = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
                f'viewBox="0 0 {WIDTH} {height}" font-family="{FONT}" role="img">\n'
                f'<title>{escape(self.title)}</title>\n<rect width="100%" height="100%" rx="6" fill="#ffffff"/>\n')
        with open(path, "w", encoding="utf-8") as f:
            f.write(head + "\n".join(self.items) + "\n</svg>\n")


def speedup_chart(sets, subtitle, labels):
    groups = []
    for s in sets:
        runs = s["runs"]
        rows = [(op, runs[op, "zip"][0] / runs[op, "parch"][0]) for op in OPS
                if (op, "zip") in runs and (op, "parch") in runs and runs[op, "parch"][0] > 0]
        if rows:
            groups.append((s["name"], rows))
    if not groups:
        return None
    svg = Svg(f"How many times parch is faster than {labels['zip']}", f"{subtitle}; dashed line: same speed")
    x0, x1 = 110, WIDTH - 70
    scale = ticks(max(v for _, rows in groups for _, v in rows))
    # Grid lines run only beside the bars, so they never cross the data set names.
    y, spans = svg.y, []
    for _, rows in groups:
        spans.append((y + 24, y + 24 + 22 * len(rows)))
        y = spans[-1][1] + 6
    bottom = spans[-1][1]
    for t in scale:
        x = x0 + (x1 - x0) * t / scale[-1]
        for y1, y2 in spans:
            svg.line(x, y1, x, y2)
        svg.text(x, bottom + 16, f"{t:g}×", size=11, anchor="middle", color=MUTED)
    one = x0 + (x1 - x0) / scale[-1]
    for y1, y2 in spans:
        svg.line(one, y1, one, y2, color=MUTED, dash=True)
    for name, rows in groups:
        svg.text(16, svg.y + 16, name, weight="bold")
        svg.y += 24
        for op, v in rows:
            w = (x1 - x0) * v / scale[-1]
            svg.text(32, svg.y + 13, op, color=MUTED)
            svg.rect(x0, svg.y + 2, w, 15, COLORS["parch"])
            svg.text(x0 + w + 6, svg.y + 14, f"{v:.1f}×" if v < 20 else f"{v:.0f}×", weight="bold")
            svg.y += 22
        svg.y += 6
    svg.y = bottom + 22
    return svg


def times_chart(sets, subtitle, labels):
    present = [t for t in COLORS if any(t == tool for s in sets for _, tool in s["runs"])]
    svg = Svg("Wall-clock time, lower is better", subtitle)
    svg.legend({t: labels[t] for t in present})
    x0, x1 = 110, WIDTH - 110
    for s in sets:
        runs = s["runs"]
        top = max(secs for secs, _ in runs.values())
        svg.text(16, svg.y + 16, s["name"], weight="bold")
        svg.y += 24
        for op in OPS:
            bars = [t for t in present if (op, t) in runs]
            if not bars:
                continue
            svg.text(32, svg.y + 12, op, color=MUTED)
            for t in bars:
                secs = runs[op, t][0]
                w = (x1 - x0) * secs / top if top else 0
                svg.rect(x0, svg.y + 2, w, 13, COLORS[t])
                svg.text(x0 + w + 6, svg.y + 13, fmt_time(secs), size=12)
                svg.y += 17
            svg.y += 7
        svg.y += 6
    return svg


def sizes_chart(sets, subtitle, zip_label):
    rows = [(s["name"], s["runs"]["create", "zip"][1], s["runs"]["create", "parch"][1]) for s in sets
            if s["runs"].get(("create", "zip"), (0, None))[1] and s["runs"].get(("create", "parch"), (0, None))[1]]
    if not rows:
        return None
    svg = Svg("Archive size, smaller is better", subtitle)
    x0, x1 = 110, WIDTH - 170
    for name, z, p in rows:
        svg.text(16, svg.y + 16, name, weight="bold")
        svg.y += 24
        diff = f" ({(p - z) / z * 100:+.1f} %)".replace("-", "−")
        for tool, label, size, note in (("zip", zip_label, z, ""), ("parch", "parch", p, diff)):
            w = (x1 - x0) * size / max(z, p)
            svg.text(32, svg.y + 12, label, color=MUTED)
            svg.rect(x0, svg.y + 2, w, 13, COLORS[tool])
            svg.text(x0 + w + 6, svg.y + 13, fmt_size(size) + note, size=12)
            svg.y += 19
        svg.y += 8
    return svg


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("logs", nargs="+", metavar="LOG")
    p.add_argument("-o", "--out", default="target/bench/charts", help="output directory")
    p.add_argument("--name", action="append", default=[], metavar="KEY=TEXT",
                   help="display name of a data set (KEY: many, big, src or a log file name)")
    p.add_argument("--zip", default="zip", metavar="LABEL", help="label of zip, e.g. 'zip 3.0'")
    p.add_argument("--unzip", default="unzip", metavar="LABEL", help="label of unzip, e.g. 'unzip 6.00'")
    a = p.parse_args()
    labels = {"zip": f"{a.zip} / {a.unzip}", "parch": "parch"}
    names = dict(n.split("=", 1) for n in a.name)
    sets, levels = parse(a.logs, names)
    if not sets:
        p.error("no benchmark results found")
    subtitle = f"compression level {levels.pop()}" if len(levels) == 1 else "parch, zip and unzip"
    os.makedirs(a.out, exist_ok=True)
    for file, chart in (("speedup.svg", speedup_chart(sets, subtitle, labels)),
                        ("times.svg", times_chart(sets, subtitle, labels)), ("sizes.svg", sizes_chart(sets, subtitle, a.zip))):
        if chart:
            chart.save(os.path.join(a.out, file))
            print(os.path.join(a.out, file))


if __name__ == "__main__":
    main()
