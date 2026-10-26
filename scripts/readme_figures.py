#!/usr/bin/env python3
"""README figures from a benchmark run (scripts/bench_pcbench.py): renders of the routed boards
and a chart of the DRC time per board, kicad-cli against copperpath's native DRC.

    python3 scripts/readme_figures.py --work bench_work [--out docs/img] \
        [--copperpath target/release/copperpath] [--kicad-cli kicad-cli]

Timings are the best of three runs on this machine; the chart is an SVG with light and dark
colours (prefers-color-scheme), values labelled directly.
"""
import argparse
import json
import os
import subprocess
import time


def best_of(cmd, n=3, timeout=None):
    """best wall time of n runs; None when a run exceeds the timeout (s)"""
    t = []
    for _ in range(n):
        t0 = time.time()
        try:
            subprocess.run(cmd, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return None
        t.append(time.time() - t0)
    return min(t)


def chart(rows, path):
    """grouped horizontal bars: per board kicad-cli and native DRC time (s), one linear axis"""
    bar, gap, group = 14, 2, 46
    left, right, top = 120, 70, 54
    w = 720
    h = top + group * len(rows) + 34
    vmax = max(r["kicad_s"] for r in rows)
    sx = lambda v: (w - left - right) * v / vmax
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" '
         'font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif" font-size="12">',
         '<style>svg{--bg:#fcfcfb;--t1:#0b0b0b;--t2:#52514e;--grid:#e4e3df;--s1:#2a78d6;--s2:#eb6834}'
         '@media (prefers-color-scheme: dark){svg{--bg:#1a1a19;--t1:#f0efec;--t2:#c3c2b7;'
         '--grid:#3a3936;--s1:#3987e5;--s2:#d95926}}'
         'text{fill:var(--t1)}.m{fill:var(--t2)}</style>',
         f'<rect width="{w}" height="{h}" rx="6" fill="var(--bg)"/>',
         f'<text x="16" y="22" font-size="14" font-weight="600">DRC time per routed board (s, lower is better)</text>']
    # legend
    lx = 16
    for name, col in [("kicad-cli pcb drc", "--s2"), ("copperpath drc (native)", "--s1")]:
        o.append(f'<rect x="{lx}" y="32" width="10" height="10" rx="2" fill="var({col})"/>')
        o.append(f'<text x="{lx + 15}" y="41" class="m">{name}</text>')
        lx += 190
    # grid
    for k in range(5):
        v = vmax * k / 4
        x = left + sx(v)
        o.append(f'<line x1="{x:.1f}" y1="{top - 4}" x2="{x:.1f}" y2="{h - 28}" stroke="var(--grid)"/>')
        o.append(f'<text x="{x:.1f}" y="{h - 12}" text-anchor="middle" class="m">{v:.1f}</text>')
    for i, r in enumerate(rows):
        y = top + i * group
        o.append(f'<text x="{left - 8}" y="{y + bar + 4}" text-anchor="end">{r["board"]}</text>')
        for j, (k, col) in enumerate([("kicad_s", "--s2"), ("native_s", "--s1")]):
            yy = y + j * (bar + gap)
            bw = max(sx(r[k]), 2)
            o.append(f'<rect x="{left}" y="{yy}" width="{bw:.1f}" height="{bar}" rx="3" fill="var({col})">'
                     f'<title>{r["board"]}: {r[k]:.3f} s</title></rect>')
            label = f'{r[k]:.2f} s' if r[k] >= 0.1 else f'{r[k] * 1000:.0f} ms'
            o.append(f'<text x="{left + bw + 6:.1f}" y="{yy + bar - 3}" class="m">{label}</text>')
    o.append("</svg>")
    open(path, "w").write("\n".join(o))


def progress(prev, now, path, prev_label, now_label):
    """grouped horizontal bars: connections left open per board, an earlier run against this one
    (input count as muted text); one linear axis"""
    rows = []
    for r in now["boards"]:
        p = next((x for x in prev["boards"] if x["board"] == r["board"]), None)
        if "error" in r or p is None or r.get("unconnected", -1) < 0:
            continue
        rows.append((r["board"], p.get("unconnected", -1), r["unconnected"], r["unconnected_in"]))
    bar, gap, group = 14, 2, 46
    # right: room for a value label after the longest bar, then the input count
    left, right, top = 120, 170, 54
    w = 720
    h = top + group * len(rows) + 34
    top_v = max(max(a, b) for _, a, b, _ in rows) or 1
    step = next(s for s in (1, 2, 5, 10, 20, 25, 50, 100, 200, 500, 1000) if top_v / s <= 5)
    vmax = step * -(-top_v // step)
    sx = lambda v: (w - left - right) * v / vmax
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" '
         'font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif" font-size="12">',
         '<style>svg{--bg:#fcfcfb;--t1:#0b0b0b;--t2:#52514e;--grid:#e4e3df;--s1:#2a78d6;--s2:#eb6834}'
         '@media (prefers-color-scheme: dark){svg{--bg:#1a1a19;--t1:#f0efec;--t2:#c3c2b7;'
         '--grid:#3a3936;--s1:#3987e5;--s2:#d95926}}'
         'text{fill:var(--t1)}.m{fill:var(--t2)}</style>',
         f'<rect width="{w}" height="{h}" rx="6" fill="var(--bg)"/>',
         '<text x="16" y="22" font-size="14" font-weight="600">Connections left open after autoroute '
         '(120 s per board, lower is better)</text>']
    lx = 16
    for name, col in [(prev_label, "--s2"), (now_label, "--s1")]:
        o.append(f'<rect x="{lx}" y="32" width="10" height="10" rx="2" fill="var({col})"/>')
        o.append(f'<text x="{lx + 15}" y="41" class="m">{name}</text>')
        lx += 190
    for k in range(int(vmax // step) + 1):
        v = k * step
        x = left + sx(v)
        o.append(f'<line x1="{x:.1f}" y1="{top - 4}" x2="{x:.1f}" y2="{h - 28}" stroke="var(--grid)"/>')
        o.append(f'<text x="{x:.1f}" y="{h - 12}" text-anchor="middle" class="m">{v:.0f}</text>')
    for i, (b, a, n, inp) in enumerate(rows):
        y = top + i * group
        o.append(f'<text x="{left - 8}" y="{y + bar + 4}" text-anchor="end">{b}</text>')
        o.append(f'<text x="{w - 12}" y="{y + bar + 4}" text-anchor="end" class="m">input: {inp}</text>')
        for j, (v, col, lab) in enumerate([(a, "--s2", prev_label), (n, "--s1", now_label)]):
            yy = y + j * (bar + gap)
            if v < 0:
                o.append(f'<text x="{left + 4}" y="{yy + bar - 3}" class="m">n/a</text>')
                continue
            bw = max(sx(v), 2)
            o.append(f'<rect x="{left}" y="{yy}" width="{bw:.1f}" height="{bar}" rx="3" fill="var({col})">'
                     f'<title>{b}, {lab}: {v} open</title></rect>')
            o.append(f'<text x="{left + bw + 6:.1f}" y="{yy + bar - 3}" class="m">{v}</text>')
    o.append("</svg>")
    open(path, "w").write("\n".join(o))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", default="bench_work")
    ap.add_argument("--out", default="docs/img")
    ap.add_argument("--copperpath", default="target/release/copperpath")
    ap.add_argument("--kicad-cli", default=os.environ.get("KICAD_CLI", "kicad-cli"))
    ap.add_argument("--kicad-timeout", type=float, default=60, help="skip a board in the chart when kicad-cli takes longer (s)")
    ap.add_argument("--prev", default="", help="results.json of an earlier run: progress chart against it")
    ap.add_argument("--prev-label", default="earlier run")
    ap.add_argument("--label", default="this run")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    run = json.load(open(os.path.join(a.work, "results.json")))
    if a.prev:
        progress(json.load(open(a.prev)), run, os.path.join(a.out, "progress.svg"), a.prev_label, a.label)
    rows = []
    for r in run["boards"]:
        if "error" in r:
            continue
        b = r["board"]
        out = os.path.join(a.work, b, f"{b}_routed.kicad_pcb")
        if not os.path.exists(out):
            continue
        n = best_of([a.copperpath, "drc", "--pcb", out])
        k = best_of([a.kicad_cli, "pcb", "drc", "--format", "json", "-o", os.path.join(a.work, "drc_fig.json"), out],
                    timeout=a.kicad_timeout)
        if k is not None:
            rows.append({"board": b, "kicad_s": k, "native_s": n})
        subprocess.run([a.copperpath, "render", "--pcb", out, "--refill-zones", "--fills", "--opens", "--px", "900",
                        "--out", os.path.join(a.out, f"bench_{b}.png")], capture_output=True)
        print(b, f"kicad {k if k is None else round(k, 2)} s native {n * 1000:.0f} ms")
    chart(rows, os.path.join(a.out, "drc_time.svg"))
    json.dump(rows, open(os.path.join(a.out, "drc_time.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
