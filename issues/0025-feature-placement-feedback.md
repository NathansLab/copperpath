# 0025 Feature request: congestion and channel-capacity feedback for placement

GitHub: https://github.com/NathansLab/copperpath/issues/12
Status: open (feature request). Related: #8.

When routing fails on dense boards, the user needs to know which placement change helps.

## Proposals
- **Pre-route capacity estimate** (global router): per channel between footprints, required versus
  available tracks per layer. Output a heat map PNG (the `img` module exists) and the top 10
  bottlenecks with location, layer and nets.
- **"Channel capacity exceeded at x, y, layer"** messages instead of a negotiation plateau.
- **Suggested moves**: for each bottleneck, the footprint whose shift by N mm frees the most
  capacity (cheap estimate from the global grid).
- **Rat's nest quality metric** (crossings, total Manhattan length) before routing, to compare
  placements quickly in `place` loops.
