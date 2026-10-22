#!/usr/bin/env python3
"""Benchmark copperpath on public PCBench boards (the set Freerouting benchmarks with).

For each board: download the original KiCad board at a pinned Freerouting commit (sha256
checked; nothing is stored in this repo), upgrade it with kicad-cli, write a .kicad_pro with the
board's own net classes (track width, clearance, via size per net) and board setup minimums,
remove all tracks and vias,
keep the zones unfilled (their nets become plane nets), run `copperpath autoroute`, then KiCad
DRC with refilled zones on the result.

    python3 scripts/bench_pcbench.py [--time 300] [--boards a,b] [--work bench_work] \
        [--copperpath target/release/copperpath] [--kicad-cli kicad-cli] [--md results.md]

Needs kicad-cli (KiCad 9 or 10) and network access to raw.githubusercontent.com.
"""
import argparse
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request

FR_COMMIT = "c4755a8874719dea76110860096776c4a9210994"
FR_URL = "https://raw.githubusercontent.com/freerouting/freerouting/{c}/scripts/benchmark/fixtures/PCBench/{b}/{f}"

# board id -> (short name, sha256 of raw.kicad_pcb); all MIT licensed (see the README table)
BOARDS = {
    "fan_controller_fan_controller": ("fan_controller", "39450c48bdfa8616"),
    "LVDS2TMDS_LVDS2TMDS": ("LVDS2TMDS", "0072717b4b3c850b"),
    "zx-sizif-128_sizif128": ("sizif128", "1772de20f67d3dd4"),
    "rxadc_14_rxadc_14": ("rxadc_14", "8eb5bb4d2ca94e1a"),
    "icehat_icehat": ("icehat", "b495e507f2e495e2"),
    "RF_SIGNAL_GENERATOR_HARDWARE_RF Signal Generator": ("rf_siggen", "5eaaf70a6ad3e3ff"),
    "a123-battery-integration_BCM": ("a123_bcm", "ed3bd8633b5f6edd"),
    "mini_mass_prog_bench_prog_rig": ("prog_rig", "e02c70dd6a227276"),
}


def fetch(board, name):
    url = FR_URL.format(c=FR_COMMIT, b=urllib.parse.quote(board), f=name)
    with urllib.request.urlopen(url, timeout=120) as r:
        return r.read()


def skip_sexpr(t, i):
    """index after the s-expression that starts at t[i] == '('"""
    depth, n = 0, len(t)
    while i < n:
        ch = t[i]
        if ch == '"':
            i += 1
            while i < n and t[i] != '"':
                i += 2 if t[i] == "\\" else 1
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def strip(t, top, anywhere):
    """drop top-level children named in `top` and nested items named in `anywhere`"""
    out, i, depth, n = [], 0, 0, len(t)
    start = 0
    while i < n:
        ch = t[i]
        if ch == '"':
            i += 1
            while i < n and t[i] != '"':
                i += 2 if t[i] == "\\" else 1
            i += 1
            continue
        if ch == "(":
            m = re.match(r"\(([a-z_]+)", t[i : i + 40])
            name = m.group(1) if m else ""
            if (depth == 1 and name in top) or (depth >= 1 and name in anywhere):
                j = i
                while j > start and t[j - 1] in " \t":
                    j -= 1
                out.append(t[start:j])
                i = skip_sexpr(t, i)
                while i < n and t[i] in " \t":
                    i += 1
                if i < n and t[i] == "\n":
                    i += 1
                start = i
                continue
            depth += 1
        elif ch == ")":
            depth -= 1
        i += 1
    out.append(t[start:])
    return "".join(out)


def net_classes(raw):
    """KiCad 5 (net_class ...) blocks -> (.kicad_pro classes, net -> class)"""
    classes, assign = [], {}
    for m in re.finditer(r"\(net_class ", raw):
        blk = raw[m.start() : skip_sexpr(raw, m.start())]
        name = re.match(r'\(net_class\s+("(?:[^"\\]|\\.)*"|\S+)', blk).group(1).strip('"')
        num = lambda k, d: float(re.search(rf"\({k} ([-\d.]+)\)", blk).group(1)) if re.search(rf"\({k} ", blk) else d
        classes.append({
            "name": name, "clearance": num("clearance", 0.2), "track_width": num("trace_width", 0.25),
            "via_diameter": num("via_dia", 0.6), "via_drill": num("via_drill", 0.3),
            "microvia_diameter": num("uvia_dia", 0.3), "microvia_drill": num("uvia_drill", 0.1),
            "diff_pair_width": num("diff_pair_width", 0.2), "diff_pair_gap": num("diff_pair_gap", 0.25),
        })
        for n in re.findall(r'\(add_net ("(?:[^"\\]|\\.)*"|[^)\s]+)\)', blk):
            assign[n.strip('"')] = [name]
    if not any(c["name"] == "Default" for c in classes):
        classes.insert(0, {"name": "Default"})
    return classes, assign


def setup_rules(raw):
    """KiCad 5 (setup ...) minimums -> .kicad_pro board.design_settings.rules (the values KiCad
    moves into the project when it opens such a board)"""
    i = raw.find("(setup")
    blk = raw[i : skip_sexpr(raw, i)] if i >= 0 else ""
    keys = {"trace_min": "min_track_width", "via_min_size": "min_via_diameter",
            "via_min_drill": "min_through_hole_diameter", "uvia_min_size": "min_microvia_diameter",
            "uvia_min_drill": "min_microvia_drill", "edge_clearance": "min_copper_edge_clearance",
            "hole_to_hole_min": "min_hole_to_hole"}
    rules = {}
    for k, v in re.findall(r"\((\w+) ([-\d.]+)\)", blk):
        if k in keys:
            rules[keys[k]] = float(v)
    return rules


def copper(t):
    """(vias, track length in mm) of a board text"""
    vias = len(re.findall(r"^\s*\(via\b", t, re.M))
    ln = 0.0
    for m in re.finditer(r"\(segment\s*\(start ([-\d.]+) ([-\d.]+)\)\s*\(end ([-\d.]+) ([-\d.]+)\)", t):
        x1, y1, x2, y2 = map(float, m.groups())
        ln += math.hypot(x2 - x1, y2 - y1)
    return vias, ln


def drc(cli, pcb, rep):
    subprocess.run([cli, "pcb", "drc", "--refill-zones", "--format", "json", "--severity-all",
                    "-o", rep, pcb], capture_output=True)
    d = json.load(open(rep))
    types = {}
    for v in d.get("violations", []):
        types[v["type"]] = types.get(v["type"], 0) + 1
    return len(d.get("unconnected_items", [])), types


def prepare(board, short, sha, work, cli):
    d = os.path.join(work, short)
    os.makedirs(d, exist_ok=True)
    raw_p = os.path.join(d, "raw.kicad_pcb")
    if not os.path.exists(raw_p):
        data = fetch(board, "raw.kicad_pcb")
        if not hashlib.sha256(data).hexdigest().startswith(sha):
            raise SystemExit(f"{board}: sha256 mismatch")
        open(raw_p, "wb").write(data)
        open(os.path.join(d, "metadata.json"), "wb").write(fetch(board, "metadata.normalized.json"))
    raw = open(raw_p, encoding="utf-8").read()
    pcb = os.path.join(d, f"{short}.kicad_pcb")
    open(pcb, "w", encoding="utf-8").write(raw)
    subprocess.run([cli, "pcb", "upgrade", "--force", pcb], capture_output=True, check=True)
    t = open(pcb, encoding="utf-8").read()
    open(pcb, "w", encoding="utf-8").write(
        strip(t, {"segment", "arc", "via"}, {"filled_polygon", "fill_segments"}))
    classes, assign = net_classes(raw)
    pro = {"meta": {"filename": f"{short}.kicad_pro", "version": 3},
           "board": {"design_settings": {"rules": setup_rules(raw)}},
           "net_settings": {"classes": classes, "netclass_assignments": assign,
                            "netclass_patterns": [], "meta": {"version": 4}}}
    json.dump(pro, open(os.path.join(d, f"{short}.kicad_pro"), "w"), indent=2)
    return pcb, json.load(open(os.path.join(d, "metadata.json")))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--time", type=float, default=300)
    ap.add_argument("--boards", default="")
    ap.add_argument("--work", default="bench_work")
    ap.add_argument("--copperpath", default="target/release/copperpath")
    ap.add_argument("--kicad-cli", default=os.environ.get("KICAD_CLI", "kicad-cli"))
    ap.add_argument("--md", default="")
    a = ap.parse_args()
    sel = [b for b, (s, _) in BOARDS.items() if not a.boards or s in a.boards.split(",")]
    rows = []
    for b in sel:
        short, sha = BOARDS[b]
        pcb, meta = prepare(b, short, sha, a.work, a.kicad_cli)
        unc0, types_in = drc(a.kicad_cli, pcb, pcb.replace(".kicad_pcb", "_drc_in.json"))
        out = pcb.replace(".kicad_pcb", "_routed.kicad_pcb")
        t0 = time.time()
        r = subprocess.run([a.copperpath, "autoroute", "--pcb", pcb, "--out", out, "--time", str(a.time),
                            "--json", "--allow-drc"], capture_output=True, text=True)
        dt = time.time() - t0
        open(pcb.replace(".kicad_pcb", "_autoroute.log"), "w").write(r.stderr)
        if r.returncode != 0 or not os.path.exists(out):
            print(f"{short}: autoroute failed ({r.returncode}): {r.stderr.strip()[-300:]}", file=sys.stderr)
            rows.append({"board": short, "error": r.stderr.strip()[-200:]})
            continue
        # the routed copy gets the project file, else kicad-cli checks it with default rules
        shutil.copy(pcb.replace(".kicad_pcb", ".kicad_pro"), out.replace(".kicad_pcb", ".kicad_pro"))
        unc, types = drc(a.kicad_cli, out, out.replace(".kicad_pcb", "_drc.json"))
        new = {k: v - types_in.get(k, 0) for k, v in types.items() if v > types_in.get(k, 0)}
        vias, ln = copper(open(out, encoding="utf-8").read())
        ref = meta["reference"]
        rows.append({
            "board": short, "layers": meta["board"]["layers"], "nets": meta["board"]["nets"],
            "unconnected_in": unc0, "unconnected": unc, "new_drc": new, "s": round(dt, 1),
            "vias": vias, "length_mm": round(ln), "ref_vias": ref["vias"],
            "ref_length_mm": round(ref["track_length_mm"]),
        })
        print(json.dumps(rows[-1]), flush=True)
    json.dump(rows, open(os.path.join(a.work, "results.json"), "w"), indent=2)
    if a.md:
        with open(a.md, "w") as f:
            f.write("| board | layers | nets | unconnected before → after | new DRC (KiCad) | time s | vias (human) | length mm (human) |\n|---|---|---|---|---|---|---|---|\n")
            for r in rows:
                if "error" in r:
                    f.write(f"| {r['board']} | | | failed | | | | |\n")
                    continue
                nd = ", ".join(f"{k} {v}" for k, v in sorted(r["new_drc"].items())) or "0"
                f.write(f"| {r['board']} | {r['layers']} | {r['nets']} | {r['unconnected_in']} → {r['unconnected']} | {nd} | {r['s']} | {r['vias']} ({r['ref_vias']}) | {r['length_mm']} ({r['ref_length_mm']}) |\n")


if __name__ == "__main__":
    main()
