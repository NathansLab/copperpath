"""Delta -> kipy item conversion and the commit flow against a mocked kipy API (no KiCad)."""

import json
import os
import sys
import unittest
from types import SimpleNamespace

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

from copperpath_kicad import delta as d  # noqa: E402


class Vector2:
    def __init__(self, x, y):
        self.x, self.y = x, y

    @classmethod
    def from_xy(cls, x, y):
        return cls(x, y)


class Item:
    def __init__(self, uuid=None):
        self.id = SimpleNamespace(value=uuid)


class Via(Item):
    def __init__(self, uuid=None):
        super().__init__(uuid)
        self.padstack = SimpleNamespace(drill=SimpleNamespace(start_layer=None, end_layer=None))


API = SimpleNamespace(
    Track=Item,
    ArcTrack=Item,
    Via=Via,
    Vector2=Vector2,
    BoardLayer=SimpleNamespace(BL_F_Cu=3, BL_In1_Cu=4, BL_B_Cu=34),
    ViaType=SimpleNamespace(VT_THROUGH=1, VT_BLIND_BURIED=2, VT_MICRO=3),
)


class Board:
    def __init__(self, nets=("SDA", "GND"), tracks=(), vias=(), fail_create=False):
        self.nets = [SimpleNamespace(name=n) for n in nets]
        self.tracks, self.vias = list(tracks), list(vias)
        self.fail_create = fail_create
        self.calls = []

    def get_nets(self):
        return self.nets

    def get_tracks(self):
        return self.tracks

    def get_vias(self):
        return self.vias

    def begin_commit(self):
        self.calls.append("begin")
        return "commit-1"

    def remove_items(self, items):
        self.calls.append(("remove", list(items)))

    def create_items(self, items):
        if self.fail_create:
            raise RuntimeError("API error")
        self.calls.append(("create", list(items)))

    def push_commit(self, commit, message):
        self.calls.append(("push", commit, message))

    def drop_commit(self, commit):
        self.calls.append(("drop", commit))


DELTA = {
    "v": 1,
    "units": "nm",
    "remove": [],
    "add": [
        {"type": "track", "net": "SDA", "layer": "F.Cu", "width": 150000,
         "start": [1000000, 2000000], "end": [3000000, 2000000]},
        {"type": "arc", "net": "SDA", "layer": "In1.Cu", "width": 150000,
         "start": [0, 0], "mid": [500000, 200000], "end": [1000000, 0]},
        {"type": "via", "net": "GND", "at": [5000000, 6000000], "size": 600000,
         "drill": 300000, "layers": ["F.Cu", "B.Cu"], "via_type": "through"},
    ],
}


def xy(v):
    return (v.x, v.y)


class LoadTest(unittest.TestCase):
    def test_accepts_v1_and_fills_defaults(self):
        delta = d.load('{"v": 1, "units": "nm"}')
        self.assertEqual(delta["add"], [])
        self.assertEqual(delta["remove"], [])

    def test_rejects_other_version_units_and_garbage(self):
        for text in ('{"v": 2, "units": "nm"}', '{"v": 1, "units": "mm"}', "[]", "not json"):
            with self.assertRaises(d.DeltaError):
                d.load(text)


class ConversionTest(unittest.TestCase):
    def setUp(self):
        self.nets = {n.name: n for n in Board().get_nets()}

    def test_track_arc_via(self):
        track, arc, via = d.to_items(API, self.nets, d.load(json.dumps(DELTA)))
        self.assertIsInstance(track, Item)
        self.assertEqual(xy(track.start), (1000000, 2000000))
        self.assertEqual(xy(track.end), (3000000, 2000000))
        self.assertEqual(track.width, 150000)
        self.assertEqual(track.layer, API.BoardLayer.BL_F_Cu)
        self.assertIs(track.net, self.nets["SDA"])

        self.assertEqual(xy(arc.mid), (500000, 200000))
        self.assertEqual(arc.layer, API.BoardLayer.BL_In1_Cu)

        self.assertIsInstance(via, Via)
        self.assertEqual(xy(via.position), (5000000, 6000000))
        self.assertEqual((via.diameter, via.drill_diameter), (600000, 300000))
        self.assertEqual(via.type, API.ViaType.VT_THROUGH)
        self.assertIs(via.net, self.nets["GND"])
        # a through via keeps KiCad's default drill span
        self.assertIsNone(via.padstack.drill.start_layer)

    def test_values_stay_integer_nanometres(self):
        item = d.to_item(API, self.nets, dict(DELTA["add"][0], width=150000.0,
                                               start=[1.0, 2.0]))
        self.assertEqual((type(item.width), type(item.start.x)), (int, int))

    def test_blind_via_falls_back_to_kicad9_type_and_sets_span(self):
        via = d.to_item(API, self.nets, dict(DELTA["add"][2], via_type="blind",
                                              layers=["F.Cu", "In1.Cu"]))
        self.assertEqual(via.type, API.ViaType.VT_BLIND_BURIED)
        self.assertEqual(via.padstack.drill.start_layer, API.BoardLayer.BL_F_Cu)
        self.assertEqual(via.padstack.drill.end_layer, API.BoardLayer.BL_In1_Cu)

    def test_netless_item_gets_no_net(self):
        item = d.to_item(API, self.nets, dict(DELTA["add"][0], net=""))
        self.assertFalse(hasattr(item, "net"))

    def test_errors(self):
        bad = [
            dict(DELTA["add"][0], net="NOPE"),
            dict(DELTA["add"][0], layer="In7.Cu"),
            dict(DELTA["add"][2], via_type="laser"),
            dict(DELTA["add"][0], type="zone"),
        ]
        for item in bad:
            with self.assertRaises(d.DeltaError):
                d.to_item(API, self.nets, item)


class ApplyTest(unittest.TestCase):
    def test_one_commit_remove_then_create(self):
        old_track, old_via = Item("uuid-t"), Via("uuid-v")
        board = Board(tracks=[old_track, Item("keep")], vias=[old_via])
        delta = d.load(json.dumps(dict(DELTA, remove=["uuid-t", "uuid-v"])))
        self.assertEqual(d.apply(board, delta, API), (2, 3))
        self.assertEqual([c if isinstance(c, str) else c[0] for c in board.calls],
                         ["begin", "remove", "create", "push"])
        self.assertEqual(board.calls[1][1], [old_track, old_via])
        self.assertEqual(len(board.calls[2][1]), 3)
        self.assertEqual(board.calls[3], ("push", "commit-1", "copperpath route"))

    def test_empty_delta_opens_no_commit(self):
        board = Board()
        self.assertEqual(d.apply(board, d.load('{"v": 1, "units": "nm"}'), API), (0, 0))
        self.assertEqual(board.calls, [])

    def test_bad_data_fails_before_the_commit(self):
        for delta in (dict(DELTA, remove=["gone"]),
                      dict(DELTA, add=[dict(DELTA["add"][0], net="NOPE")])):
            board = Board()
            with self.assertRaises(d.DeltaError):
                d.apply(board, d.load(json.dumps(delta)), API)
            self.assertEqual(board.calls, [])

    def test_api_error_drops_the_commit(self):
        board = Board(fail_create=True)
        with self.assertRaises(RuntimeError):
            d.apply(board, d.load(json.dumps(DELTA)), API)
        self.assertEqual(board.calls, ["begin", ("drop", "commit-1")])


if __name__ == "__main__":
    unittest.main()
