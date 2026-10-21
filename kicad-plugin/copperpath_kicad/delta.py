"""Turn a copperpath `--delta-json` change set into kipy board items and apply it as one commit.

Nothing here imports kipy at module level: the kipy classes come in through an `api` object
(see `kipy_api()`), so the conversion can be tested against a mocked API without KiCad.
"""

import json
from types import SimpleNamespace

SCHEMA_VERSION = 1

# delta `via_type` -> kipy ViaType names, first one that exists wins (KiCad 10 split blind and
# buried vias, KiCad 9 only has VT_BLIND_BURIED)
VIA_TYPES = {
    "through": ("VT_THROUGH",),
    "blind": ("VT_BLIND", "VT_BLIND_BURIED"),
    "buried": ("VT_BURIED", "VT_BLIND_BURIED"),
    "micro": ("VT_MICRO",),
}


class DeltaError(Exception):
    """The delta cannot be applied to this board (bad schema, unknown net/layer, stale UUID)."""


def kipy_api():
    """The kipy classes the conversion needs (import only inside KiCad's plugin environment)."""
    from kipy.board_types import ArcTrack, Track, Via
    from kipy.geometry import Vector2
    from kipy.proto.board.board_types_pb2 import BoardLayer, ViaType

    return SimpleNamespace(
        Track=Track,
        ArcTrack=ArcTrack,
        Via=Via,
        Vector2=Vector2,
        BoardLayer=BoardLayer,
        ViaType=ViaType,
    )


def load(text):
    """Parse and check a delta document (schema version 1, integer nanometres)."""
    try:
        delta = json.loads(text)
    except ValueError as e:
        raise DeltaError(f"delta is not valid JSON: {e}") from e
    if not isinstance(delta, dict):
        raise DeltaError("delta is not a JSON object")
    if delta.get("v") != SCHEMA_VERSION:
        raise DeltaError(
            f"delta schema version {delta.get('v')!r}, this plugin understands {SCHEMA_VERSION}; "
            "update the plugin or copperpath so both match"
        )
    if delta.get("units") != "nm":
        raise DeltaError(f"delta units {delta.get('units')!r}, expected 'nm'")
    delta.setdefault("remove", [])
    delta.setdefault("add", [])
    return delta


def layer_id(api, name):
    """KiCad layer name ("F.Cu", "In1.Cu", "B.Cu") -> kipy BoardLayer value."""
    value = getattr(api.BoardLayer, "BL_" + name.replace(".", "_"), None)
    if value is None:
        raise DeltaError(f"unknown copper layer {name!r}")
    return value


def via_type(api, name):
    for attr in VIA_TYPES.get(name, ()):
        value = getattr(api.ViaType, attr, None)
        if value is not None:
            return value
    raise DeltaError(f"unknown via type {name!r}")


def point(api, xy):
    x, y = xy
    return api.Vector2.from_xy(int(x), int(y))


def net_of(nets, item):
    name = item.get("net")
    if not name:
        return None
    if name not in nets:
        raise DeltaError(f"net {name!r} is not on the board in the editor")
    return nets[name]


def to_item(api, nets, d):
    """One delta `add` entry -> one kipy board item."""
    kind = d.get("type")
    if kind == "track":
        it = api.Track()
        it.start = point(api, d["start"])
        it.end = point(api, d["end"])
    elif kind == "arc":
        it = api.ArcTrack()
        it.start = point(api, d["start"])
        it.mid = point(api, d["mid"])
        it.end = point(api, d["end"])
    elif kind == "via":
        it = api.Via()
        it.position = point(api, d["at"])
        it.diameter = int(d["size"])
        it.drill_diameter = int(d["drill"])
        it.type = via_type(api, d.get("via_type", "through"))
        layers = d.get("layers") or []
        if it.type != getattr(api.ViaType, "VT_THROUGH", None) and len(layers) == 2:
            drill = it.padstack.drill
            drill.start_layer = layer_id(api, layers[0])
            drill.end_layer = layer_id(api, layers[1])
    else:
        raise DeltaError(f"unknown item type {kind!r}")
    if kind != "via":
        it.width = int(d["width"])
        it.layer = layer_id(api, d["layer"])
    net = net_of(nets, d)
    if net is not None:
        it.net = net
    return it


def to_items(api, nets, delta):
    """All `add` entries in output order; `nets` maps net name -> kipy Net."""
    return [to_item(api, nets, d) for d in delta["add"]]


def removed_items(board, uuids):
    """Board tracks/arcs/vias whose UUID is in `uuids`; every UUID must be found."""
    if not uuids:
        return []
    want = set(uuids)
    found = [
        it
        for it in list(board.get_tracks()) + list(board.get_vias())
        if str(it.id.value) in want
    ]
    missing = want - {str(it.id.value) for it in found}
    if missing:
        raise DeltaError(
            f"{len(missing)} item(s) to remove are no longer on the board "
            f"(e.g. {sorted(missing)[0]}); the board changed while routing, run again"
        )
    return found


def apply(board, delta, api, message="copperpath route"):
    """Apply the delta to the board as one commit, so one Ctrl+Z undoes all of it.

    Everything that can fail on bad data (nets, layers, UUIDs) is checked before the commit is
    opened; if KiCad rejects the change, the commit is dropped and nothing stays half applied.
    Returns (removed, added) item counts.
    """
    nets = {n.name: n for n in board.get_nets()}
    items = to_items(api, nets, delta)
    gone = removed_items(board, delta["remove"])
    if not items and not gone:
        return 0, 0
    commit = board.begin_commit()
    try:
        if gone:
            board.remove_items(gone)
        if items:
            board.create_items(items)
    except BaseException:
        board.drop_commit(commit)
        raise
    board.push_commit(commit, message)
    return len(gone), len(items)
