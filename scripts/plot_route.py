#!/usr/bin/env python3
"""Plot the copper of a region: pads (from `placeroute pads`), tracks and vias per layer.
Locked copper is drawn thin and pale, unlocked (router) copper in full colour.

usage: plot_route.py OUT.png x0,y0,x1,y1 TITLE=BOARD.kicad_pcb [TITLE=BOARD.kicad_pcb ...]

Requires Python 3.9+ and matplotlib. Pad outlines come from `placeroute pads`; the binary is
$PLACEROUTE_BIN, else target/release/placeroute of this repository (run `cargo build --release`).
"""
import os
import re
import subprocess
import sys
from pathlib import Path

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.patches import Circle, Polygon
import math

BIN = os.environ.get('PLACEROUTE_BIN') or Path(__file__).resolve().parent.parent / 'target/release/placeroute'
COL = {'F.Cu': '#d62728', 'In1.Cu': '#9467bd', 'In2.Cu': '#8c564b', 'In3.Cu': '#e377c2', 'In4.Cu': '#17becf', 'B.Cu': '#1f77b4'}
SEG_L = re.compile(r'\(segment\s*(\(locked yes\)\s*)?\(start ([-\d.]+) ([-\d.]+)\)\s*\(end ([-\d.]+) ([-\d.]+)\)\s*\(width ([-\d.]+)\)\s*(\(locked yes\)\s*)?\(layer "([^"]+)"\)', re.S)
VIA = re.compile(r'\(via\s*(\(locked yes\)\s*)?\(at ([-\d.]+) ([-\d.]+)\)\s*\(size ([-\d.]+)\)', re.S)


def draw(ax, pcb, box, title):
    x0, y0, x1, y1 = box
    inb = lambda x, y: x0 - 1 <= x <= x1 + 1 and y0 - 1 <= y <= y1 + 1
    pads = subprocess.run([str(BIN), 'pads', '--pcb', pcb], capture_output=True, text=True, check=True).stdout
    for ln in pads.splitlines():
        f = ln.split('\t')
        if len(f) < 7:
            continue
        x, y, r, w, h = map(float, f[2:7])
        if not inb(x, y):
            continue
        c, s = math.cos(math.radians(-r)), math.sin(math.radians(-r))
        pts = [(x + dx * c - dy * s, y + dx * s + dy * c) for dx, dy in ((-w / 2, -h / 2), (w / 2, -h / 2), (w / 2, h / 2), (-w / 2, h / 2))]
        ax.add_patch(Polygon(pts, closed=True, fc='#bbbbbb', ec='#666666', lw=0.3, zorder=1))
    t = Path(pcb).read_text()
    for m in SEG_L.finditer(t):
        lk = bool(m.group(1) or m.group(7))
        xa, ya, xb, yb, w = map(float, m.group(2, 3, 4, 5, 6))
        if not (inb(xa, ya) or inb(xb, yb)):
            continue
        col = COL.get(m.group(8), 'k')
        # linewidth in points from mm at the figure scale
        lw = w * ax._mm2pt
        ax.plot([xa, xb], [ya, yb], color=col, lw=lw, alpha=0.25 if lk else 0.85, solid_capstyle='round', zorder=2 if lk else 3)
    for m in VIA.finditer(t):
        lk = bool(m.group(1))
        x, y, d = map(float, m.group(2, 3, 4))
        if inb(x, y):
            ax.add_patch(Circle((x, y), d / 2, fc='#f0c000' if not lk else '#e8e0b0', ec='k', lw=0.3, zorder=4))
    ax.set_xlim(x0, x1)
    ax.set_ylim(y1, y0)
    ax.set_aspect('equal')
    ax.set_title(title, fontsize=9)
    ax.tick_params(labelsize=6)


def main():
    out, box = sys.argv[1], tuple(map(float, sys.argv[2].split(',')))
    panels = [a.split('=', 1) for a in sys.argv[3:]]
    wmm = box[2] - box[0]
    fig, axs = plt.subplots(1, len(panels), figsize=(5.2 * len(panels), 5.2 * (box[3] - box[1]) / wmm + 0.6), dpi=200)
    axs = [axs] if len(panels) == 1 else list(axs)
    for ax, (title, pcb) in zip(axs, panels):
        fig.canvas.draw()
        bb = ax.get_window_extent()
        ax._mm2pt = bb.width / wmm * 72 / fig.dpi
        draw(ax, pcb, box, title)
    hs = [plt.Line2D([], [], color=c, lw=2, label=l) for l, c in COL.items()]
    fig.legend(handles=hs, loc='lower center', ncol=6, fontsize=7, frameon=False)
    fig.tight_layout(rect=(0, 0.04, 1, 1))
    fig.savefig(out)


if __name__ == '__main__':
    main()
