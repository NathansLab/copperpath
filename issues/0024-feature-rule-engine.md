# 0024 Feature request: more hard and soft rules (rule engine)

GitHub: https://github.com/NathansLab/copperpath/issues/11
Status: open (feature request). Related: #2 (DRU intersectsCourtyard).

copperpath reads net classes and part of the KiCad DRU language. Real boards need more, and some
rules are preferences, not legality.

## Hard rules (must hold; the result is illegal otherwise)
- **Full DRU condition support**: `A.intersectsCourtyard()`, `intersectsArea()`, `A.NetClass`,
  `A.Type`, `A.Layer`, `memberOfGroup`, `memberOfFootprint`, evaluated per position (#2).
- **HV creepage/clearance by voltage**: clearance from a net-pair voltage table (IPC-2221B /
  IEC 60664 style) instead of only net classes; separate outer/inner values; slots count as
  creepage paths (KiCad 9+ `creepage` rule).
- **Keep-out by net class** (rule areas that block only some classes).
- **Max via count / no via per net** (e.g. sensitive analog, RF).
- **Layer restrictions per net or class** (`allowed_layers`), e.g. HV only on outer layers.
- **Differential pairs** (`_P/_N`, `+/-`): coupled routing with gap and uncoupled length limit.
- **Length / skew limits** (`length` and `skew` DRU constraints) with meanders.
- **Net ties** respected (net-tie footprints join nets locally only).

## Soft rules (cost terms, tunable weights, reported as scores)
- Preferred direction per layer (exists as `wrong_dir`; expose per layer in the config).
- Straight exit from pads (stub rule exists; make it a named soft rule with weight).
- Short distance to the reference plane / no routing over plane splits.
- Keep analog and switching nets apart (`separation` soft clearance between classes).
- Kelvin connections: sense nets leave the pad from a defined side / point (config per footprint
  pad pair), not via the power copper.
- Minimise vias on power and high-current nets, prefer pours.
- Symmetry for repeated channels (same topology in each copy of a hierarchical sheet).

## Mechanism
- One rule table internally: each rule is either hard (legality maps / exact checker) or soft
  (A* cost term with a weight), loaded from DRU + a `copperpath` config block.
- `--json` reports per soft rule: total penalty and the worst items, so weights can be tuned.

## Acceptance (first step)
Diff pairs on a synthetic USB board route coupled within 0.1 mm gap tolerance, and an
`allowed_layers` rule keeps one class on F.Cu/B.Cu; tests for both.
