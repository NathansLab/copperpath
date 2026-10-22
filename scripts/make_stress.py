#!/usr/bin/env python3
"""Synthetic stress board for copperpath benchmarks (no KiCad needed).

    python scripts/make_stress.py OUT.kicad_pcb [--parts 800] [--layers 6] [--seed 1]

A grid of SOIC-8 ICs, each surrounded by 0603 passives: pin 4 of every IC is GND, pin 8 is
+3V3, the other pins go to nearby passives; passives chain to their neighbours or to GND/+3V3.
With 6 layers, GND gets a zone on In1 and +3V3 one on In4 (unfilled). Deterministic per seed.
"""
import random
import sys


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        sys.exit(2)
    out = args[0]
    opt = {k: v for k, v in zip(args[1::2], args[2::2])}
    parts = int(opt.get("--parts", 800))
    layers = int(opt.get("--layers", 6))
    rnd = random.Random(int(opt.get("--seed", 1)))
    # one cluster = 1 IC + 18 passives in a 14 x 10 mm cell
    clusters = max(1, parts // 19)
    cols = int(clusters ** 0.5 + 0.999)
    rows = (clusters + cols - 1) // cols
    cw, ch = 14.0, 10.0
    x0, y0 = 100.0, 100.0
    W, H = cols * cw + 4, rows * ch + 4
    fps = []
    nets = set(["GND", "+3V3"])
    n_sig = [0]

    def sig():
        n_sig[0] += 1
        name = f"/S{n_sig[0]}"
        nets.add(name)
        return name

    def soic(ref, x, y, pn):
        pads = []
        for i in range(8):
            side = -1 if i < 4 else 1
            k = i if i < 4 else 7 - i
            px, py = side * 2.7, -1.905 + k * 1.27
            pads.append(
                f'(pad "{i + 1}" smd roundrect (at {px:.3f} {py:.3f}) (size 1.5 0.6) '
                f'(layers "F.Cu" "F.Mask" "F.Paste") (roundrect_rratio 0.25) (net "{pn[i]}"))'
            )
        return (
            f'(footprint "Package_SO:SOIC-8" (layer "F.Cu") (at {x:.3f} {y:.3f}) '
            f'(property "Reference" "{ref}") '
            f'(fp_rect (start -3.7 -2.7) (end 3.7 2.7) (stroke (width 0.05)) (layer "F.CrtYd")) '
            + " ".join(pads) + ")"
        )

    def r0603(ref, x, y, rot, a, b):
        return (
            f'(footprint "R_0603" (layer "F.Cu") (at {x:.3f} {y:.3f} {rot}) '
            f'(property "Reference" "{ref}") '
            f'(fp_rect (start -1.5 -0.75) (end 1.5 0.75) (stroke (width 0.05)) (layer "F.CrtYd")) '
            f'(pad "1" smd roundrect (at -0.775 0 {rot}) (size 0.9 0.95) (layers "F.Cu" "F.Mask" "F.Paste") (roundrect_rratio 0.25) (net "{a}")) '
            f'(pad "2" smd roundrect (at 0.775 0 {rot}) (size 0.9 0.95) (layers "F.Cu" "F.Mask" "F.Paste") (roundrect_rratio 0.25) (net "{b}")))'
        )

    nr = 0
    for c in range(clusters):
        cx = x0 + 2 + (c % cols) * cw + cw / 2
        cy = y0 + 2 + (c // cols) * ch + ch / 2
        pn = [sig(), sig(), sig(), "GND", sig(), sig(), sig(), "+3V3"]
        fps.append(soic(f"U{c + 1}", cx, cy, pn))
        # 18 passives around the IC: a column of 5 left and right, a row of 4 above and below
        spots = []
        for k in range(5):
            spots.append((cx - 5.5, cy - 2.8 + k * 1.4, 0))
            spots.append((cx + 5.5, cy - 2.8 + k * 1.4, 0))
        for k in range(4):
            spots.append((cx - 3.75 + k * 2.5, cy - 4.2, 0))
            spots.append((cx - 3.75 + k * 2.5, cy + 4.2, 0))
        ic_sig = [s for s in pn if s.startswith("/")]
        prev = None
        for k, (x, y, rot) in enumerate(spots):
            nr += 1
            a = ic_sig[k] if k < len(ic_sig) else (prev or sig())
            u = rnd.random()
            b = "GND" if u < 0.35 else ("+3V3" if u < 0.5 else sig())
            prev = b if b.startswith("/") else None
            fps.append(r0603(f"R{nr}", x, y, rot, a, b))
    # drop single-pin signal nets by tying them to GND
    count = {}
    for f in fps:
        for n in __import__("re").findall(r'\(net "([^"]+)"\)', f):
            count[n] = count.get(n, 0) + 1
    fps = [
        __import__("re").sub(
            r'\(net "(/S\d+)"\)',
            lambda m: m.group(0) if count[m.group(1)] > 1 else '(net "GND")',
            f,
        )
        for f in fps
    ]
    lay = '(0 "F.Cu" signal) (2 "B.Cu" signal)'
    inner = ["In1.Cu", "In2.Cu", "In3.Cu", "In4.Cu"][: max(0, layers - 2)]
    for i, l in enumerate(inner):
        lay += f' ({4 + 2 * i} "{l}" signal)'
    zones = ""
    x1, y1 = x0 + W, y0 + H
    poly = f"(polygon (pts (xy {x0 + 0.5} {y0 + 0.5}) (xy {x1 - 0.5} {y0 + 0.5}) (xy {x1 - 0.5} {y1 - 0.5}) (xy {x0 + 0.5} {y1 - 0.5})))"
    if layers >= 6:
        zones += f'(zone (net "GND") (layer "In1.Cu") (name "GND_In1") (min_thickness 0.25) {poly})'
        zones += f'(zone (net "+3V3") (layer "In4.Cu") (name "P3V3_In4") (min_thickness 0.25) {poly})'
    text = (
        f'(kicad_pcb (version 20260206) (generator "make_stress") (layers {lay} (25 "Edge.Cuts" user) '
        f'(39 "F.Mask" user) (40 "B.Mask" user) (31 "F.CrtYd" user)) '
        f'(gr_rect (start {x0} {y0}) (end {x1} {y1}) (stroke (width 0.1)) (layer "Edge.Cuts")) '
        + "\n".join(fps) + zones + ")\n"
    )
    open(out, "w").write(text)
    print(f"{out}: {len(fps)} footprints, {len(nets)} nets, {W:.0f} x {H:.0f} mm, {layers} layers")


if __name__ == "__main__":
    main()
