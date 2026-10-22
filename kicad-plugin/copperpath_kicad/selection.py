"""The editor selection -> what to route or rip up (`--nets`, `--region`, items to remove).

kipy items are told apart by class name, so the functions work on mocked items without KiCad.
"""

# items whose own net is selected
NET_ITEMS = {"Track", "ArcTrack", "Via", "Pad"}
# footprints select the nets of all their pads
FOOTPRINTS = {"FootprintInstance", "Footprint"}
# graphic shapes and zones whose bounding box is a region (draw a rectangle on a User layer)
REGION_ITEMS = {
    "BoardRectangle",
    "BoardPolygon",
    "BoardCircle",
    "BoardSegment",
    "BoardArc",
    "BoardBezier",
    "BoardShape",
    "Zone",
}
NM_PER_MM = 1_000_000


class SelectionError(Exception):
    """The selection does not fit the command (nothing usable selected, unusable net name)."""


def kind(item):
    return type(item).__name__


def net_name(item):
    net = getattr(item, "net", None)
    return getattr(net, "name", "") or ""


def selected_nets(items):
    """Sorted names of the nets of the selected tracks, arcs, vias, pads and footprints."""
    nets = set()
    for it in items:
        k = kind(it)
        if k in NET_ITEMS:
            nets.add(net_name(it))
        elif k in FOOTPRINTS:
            definition = getattr(it, "definition", None)
            nets.update(net_name(p) for p in getattr(definition, "pads", ()))
    nets.discard("")
    if not nets:
        raise SelectionError(
            "no net selected: select tracks, vias, pads or footprints of the nets first"
        )
    return sorted(nets)


def nets_arg(nets):
    """`--nets` value: the names joined by commas (copperpath splits the list at commas)."""
    bad = [n for n in nets if "," in n]
    if bad:
        raise SelectionError(f"net name {bad[0]!r} contains a comma and cannot be passed to --nets")
    return ",".join(nets)


def item_box(board, item):
    """(x0, y0, x1, y1) in nm of one item: KiCad's bounding box, else the rectangle corners."""
    get = getattr(board, "get_item_bounding_box", None)
    if get is not None:
        box = get(item)
        if isinstance(box, (list, tuple)):
            box = box[0] if box else None
        if box is not None:
            x, y = box.pos.x, box.pos.y
            return x, y, x + box.size.x, y + box.size.y
    a, b = getattr(item, "top_left", None), getattr(item, "bottom_right", None)
    if a is not None and b is not None:
        return min(a.x, b.x), min(a.y, b.y), max(a.x, b.x), max(a.y, b.y)
    return None


def selected_region(board, items):
    """Bounding box (x0, y0, x1, y1) in mm of the selected shapes and zones."""
    boxes = [item_box(board, it) for it in items if kind(it) in REGION_ITEMS]
    boxes = [b for b in boxes if b is not None]
    if not boxes:
        raise SelectionError(
            "no region selected: draw a rectangle on a User layer (or select a zone) first"
        )
    x0, y0 = min(b[0] for b in boxes), min(b[1] for b in boxes)
    x1, y1 = max(b[2] for b in boxes), max(b[3] for b in boxes)
    if x1 <= x0 or y1 <= y0:
        raise SelectionError("the selected region has no area")
    return tuple(v / NM_PER_MM for v in (x0, y0, x1, y1))


def region_arg(box):
    """`--region` value in mm."""
    return ",".join(f"{v:.6f}" for v in box)


def ripup_items(board, nets):
    """(unlocked tracks, arcs and vias of `nets`, number of locked ones that stay)."""
    want = set(nets)
    items, locked = [], 0
    for it in list(board.get_tracks()) + list(board.get_vias()):
        if net_name(it) not in want:
            continue
        if getattr(it, "locked", False):
            locked += 1
        else:
            items.append(it)
    return items, locked
