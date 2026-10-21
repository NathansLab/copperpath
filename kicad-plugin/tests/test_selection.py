"""Selection -> --nets / --region / rip-up and the selection commands, against mocked kipy items."""

import os
import sys
import unittest
from types import SimpleNamespace
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [os.path.join(HERE, ".."), HERE]

from copperpath_kicad import action, runner, selection  # noqa: E402

from test_delta import API, DELTA  # noqa: E402
from test_runner import EditorBoard  # noqa: E402


def net(name):
    return SimpleNamespace(name=name)


def vec(x, y):
    return SimpleNamespace(x=x, y=y)


# mocked kipy item classes (told apart by class name like the real ones)
class Track:
    def __init__(self, n, uuid="", locked=False):
        self.net, self.id, self.locked = net(n), SimpleNamespace(value=uuid), locked


class Via(Track):
    pass


class Pad:
    def __init__(self, n):
        self.net = net(n)


class FootprintInstance:
    def __init__(self, *nets):
        self.definition = SimpleNamespace(pads=[Pad(n) for n in nets])


class BoardRectangle:
    def __init__(self, x0, y0, x1, y1):
        self.top_left, self.bottom_right = vec(x0, y0), vec(x1, y1)


class Zone:
    def __init__(self, x, y, w, h):
        self.box = SimpleNamespace(pos=vec(x, y), size=vec(w, h))


class BoardText:
    pass


class SelBoard(EditorBoard):
    """Editor board with a selection; Zone boxes come from get_item_bounding_box."""

    def __init__(self, selection=(), **kw):
        super().__init__("", **kw)
        self.selection = list(selection)

    def get_selection(self):
        return self.selection

    def get_item_bounding_box(self, item):
        return getattr(item, "box", None)


MM = 1_000_000


class SelectionTest(unittest.TestCase):
    def test_nets_of_tracks_vias_pads_and_footprints(self):
        items = [Track("SDA"), Via("GND"), Pad("SCL"), FootprintInstance("+5V", "", "GND"),
                 BoardText(), Pad("")]
        self.assertEqual(selection.selected_nets(items), ["+5V", "GND", "SCL", "SDA"])

    def test_no_net_selected(self):
        for items in ([], [BoardText()], [Pad("")], [BoardRectangle(0, 0, 1, 1)]):
            with self.assertRaises(selection.SelectionError):
                selection.selected_nets(items)

    def test_nets_arg_refuses_commas(self):
        self.assertEqual(selection.nets_arg(["/a", "GND"]), "/a,GND")
        with self.assertRaises(selection.SelectionError):
            selection.nets_arg(["A,B"])

    def test_region_is_the_union_of_shapes_and_zones_in_mm(self):
        board = SelBoard()
        items = [BoardRectangle(30 * MM, 20 * MM, 10 * MM, 5 * MM),
                 Zone(40 * MM, 1 * MM, 5 * MM, 2 * MM), Track("SDA")]
        box = selection.selected_region(board, items)
        self.assertEqual(box, (10.0, 1.0, 45.0, 20.0))
        self.assertEqual(selection.region_arg(box), "10.000000,1.000000,45.000000,20.000000")

    def test_no_region_selected(self):
        board = SelBoard()
        for items in ([], [Track("SDA")], [BoardRectangle(1, 1, 1, 5)]):
            with self.assertRaises(selection.SelectionError):
                selection.selected_region(board, items)

    def test_ripup_keeps_locked_and_other_nets(self):
        t1, t2, t3, v1 = Track("SDA", "t1"), Track("SDA", "t2", locked=True), Track("GND"), \
            Via("SDA", "v1")
        board = SelBoard(tracks=[t1, t2, t3], vias=[v1])
        self.assertEqual(selection.ripup_items(board, ["SDA"]), ([t1, v1], 1))


def kinds(board):
    return [c if isinstance(c, str) else c[0] for c in board.calls]


class CommandTest(unittest.TestCase):
    def run_cmd(self, board, command, delta=DELTA, settings=None):
        kicad = SimpleNamespace(get_board=lambda: board)
        with mock.patch.object(runner, "find_binary", return_value="cp"), \
                mock.patch.object(runner, "route", return_value=delta) as route, \
                mock.patch.object(runner, "load_settings", return_value=settings or {}), \
                mock.patch.object(action.ui, "error") as dialog:
            code = action.run(kicad, API, command)
        args = route.call_args[0][4] if route.called else None
        msg = dialog.call_args[0][0] if dialog.called else None
        return code, args, msg

    def test_route_selected_nets(self):
        board = SelBoard([Pad("SDA"), Track("GND")])
        code, args, _ = self.run_cmd(board, "nets")
        self.assertEqual(code, 0)
        self.assertEqual(args, ["--nets", "GND,SDA"])
        self.assertEqual(kinds(board), ["begin", "create", "push"])

    def test_copper_outside_the_selection_is_refused(self):
        board = SelBoard([Pad("SDA")])
        code, _, msg = self.run_cmd(board, "nets")
        self.assertEqual(code, 1)
        self.assertIn("outside the selection", msg)
        self.assertEqual(board.calls, [])

    def test_route_region(self):
        board = SelBoard([BoardRectangle(0, 0, 10 * MM, 5 * MM)])
        code, args, _ = self.run_cmd(board, "region", settings={"args": ["--time-limit", 9]})
        self.assertEqual(code, 0)
        self.assertEqual(args, ["--time-limit", "9", "--region",
                                "0.000000,0.000000,10.000000,5.000000"])

    def test_route_all_needs_no_selection_and_never_removes(self):
        board = SelBoard(tracks=[Track("SDA", "manual")])
        code, args, _ = self.run_cmd(board, "all")
        self.assertEqual((code, args), (0, []))
        code, _, msg = self.run_cmd(board, "all", delta=dict(DELTA, remove=["manual"]))
        self.assertEqual(code, 1)
        self.assertIn("remove", msg)
        self.assertEqual(kinds(board), ["begin", "create", "push"])

    def test_ignore_unlocked_setting_is_refused(self):
        board = SelBoard()
        code, args, msg = self.run_cmd(board, "all", settings={"args": ["--ignore-unlocked"]})
        self.assertEqual((code, args), (1, None))
        self.assertIn("--ignore-unlocked", msg)

    def test_empty_selection_is_a_dialog(self):
        for command in ("nets", "region", "ripup", "ripup-route"):
            board = SelBoard()
            code, args, msg = self.run_cmd(board, command)
            self.assertEqual((code, args, board.calls), (1, None, []))
            self.assertIn("select", msg)

    def test_ripup_is_its_own_commit(self):
        t1, keep = Track("SDA", "t1"), Track("GND", "t2")
        board = SelBoard([t1], tracks=[t1, keep])
        code, args, _ = self.run_cmd(board, "ripup")
        self.assertEqual((code, args), (0, None))
        self.assertEqual(board.calls, ["begin", ("remove", [t1]),
                                       ("push", "commit-1", "copperpath rip up")])

    def test_ripup_then_route_two_commits(self):
        t1 = Track("SDA", "t1")
        board = SelBoard([t1, Via("GND")], tracks=[t1])
        code, args, _ = self.run_cmd(board, "ripup-route")
        self.assertEqual(code, 0)
        self.assertEqual(args, ["--nets", "GND,SDA"])
        self.assertEqual(kinds(board), ["begin", "remove", "push", "begin", "create", "push"])
        self.assertEqual(board.calls[2][2], "copperpath rip up")
        self.assertEqual(board.calls[5][2], "copperpath route")

    def test_bad_settings_stop_before_the_ripup(self):
        t1 = Track("SDA", "t1")
        board = SelBoard([t1], tracks=[t1])
        code, args, _ = self.run_cmd(board, "ripup-route", settings={"args": ["--nets", "X"]})
        self.assertEqual((code, args, board.calls), (1, None, []))


if __name__ == "__main__":
    unittest.main()
