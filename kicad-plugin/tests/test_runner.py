"""Binary lookup, subprocess handling and the action flow, without KiCad or copperpath."""

import json
import os
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [os.path.join(HERE, ".."), HERE]

from copperpath_kicad import action, runner  # noqa: E402

from test_delta import API, DELTA, Board  # noqa: E402


def touch(path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write("")
    return path


class FindBinaryTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def test_setting_then_bundled_then_path(self):
        on_path = lambda name: "/usr/bin/copperpath"  # noqa: E731
        self.assertEqual(runner.find_binary(self.dir, {}, on_path), "/usr/bin/copperpath")
        bundled = touch(os.path.join(self.dir, "bin", runner.EXE))
        self.assertEqual(runner.find_binary(self.dir, {}, on_path), bundled)
        own = touch(os.path.join(self.dir, "my", "copperpath"))
        self.assertEqual(runner.find_binary(self.dir, {"binary": own}, on_path), own)

    def test_missing(self):
        with self.assertRaises(runner.RouteError):
            runner.find_binary(self.dir, {}, lambda name: None)
        with self.assertRaises(runner.RouteError):
            runner.find_binary(self.dir, {"binary": os.path.join(self.dir, "nope")})

    def test_settings_file(self):
        self.assertEqual(runner.load_settings(self.dir), {})
        with open(os.path.join(self.dir, runner.SETTINGS), "w", encoding="utf-8") as f:
            json.dump({"binary": "x", "args": ["--time-limit", "60"]}, f)
        self.assertEqual(runner.load_settings(self.dir)["args"], ["--time-limit", "60"])
        with open(os.path.join(self.dir, runner.SETTINGS), "w", encoding="utf-8") as f:
            f.write("{broken")
        with self.assertRaises(runner.RouteError):
            runner.load_settings(self.dir)


class RouteTest(unittest.TestCase):
    def test_command(self):
        cmd = runner.build_command("cp", "d.json", "b.kicad_pro", None, ["--time-limit", 60])
        self.assertEqual(cmd, ["cp", "route", "--pcb", "-", "--delta-json", "d.json",
                               "--pro", "b.kicad_pro", "--time-limit", "60"])

    def fake_run(self, code=0, delta=DELTA, stderr=b"[route] done\n"):
        seen = {}

        def run(cmd, input=None, **kw):
            seen["cmd"], seen["input"] = cmd, input
            if delta is not None:
                with open(cmd[cmd.index("--delta-json") + 1], "w", encoding="utf-8") as f:
                    json.dump(delta, f)
            return SimpleNamespace(returncode=code, stdout=b"", stderr=stderr)

        return run, seen

    def test_board_text_on_stdin_and_delta_parsed(self):
        run, seen = self.fake_run()
        delta = runner.route("cp", "(kicad_pcb)", run=run)
        self.assertEqual(seen["input"], b"(kicad_pcb)")
        self.assertEqual(seen["cmd"][:4], ["cp", "route", "--pcb", "-"])
        self.assertEqual(len(delta["add"]), 3)

    def test_nonzero_exit_reports_log_tail(self):
        log = "".join(f"line {i}\n" for i in range(50)).encode()
        run, _ = self.fake_run(code=2, delta=None, stderr=log)
        with self.assertRaises(runner.RouteError) as e:
            runner.route("cp", "", run=run)
        self.assertIn("code 2", str(e.exception))
        self.assertEqual(e.exception.log.splitlines()[-1], "line 49")
        self.assertEqual(len(e.exception.log.splitlines()), runner.LOG_LINES)

    def test_no_or_bad_delta(self):
        for delta in (None, {"v": 99, "units": "nm"}):
            run, _ = self.fake_run(delta=delta)
            with self.assertRaises(runner.RouteError):
                runner.route("cp", "", run=run)

    def test_binary_cannot_start(self):
        def run(cmd, **kw):
            raise FileNotFoundError(cmd[0])

        with self.assertRaises(runner.RouteError):
            runner.route("cp", "", run=run)


class EditorBoard(Board):
    def __init__(self, folder, **kw):
        super().__init__(**kw)
        self.name = "demo.kicad_pcb"
        self.folder = folder

    def get_as_string(self):
        return "(kicad_pcb demo)"

    def get_project(self):
        return SimpleNamespace(path=self.folder)


class ActionTest(unittest.TestCase):
    def test_routes_the_editor_board_in_one_commit(self):
        with tempfile.TemporaryDirectory() as folder:
            pro = touch(os.path.join(folder, "demo.kicad_pro"))
            board = EditorBoard(folder)
            kicad = SimpleNamespace(get_board=lambda: board)
            with mock.patch.object(runner, "find_binary", return_value="cp"), \
                    mock.patch.object(runner, "route", return_value=DELTA) as route, \
                    mock.patch.object(runner, "load_settings", return_value={}):
                self.assertEqual(action.run(kicad, API), 0)
            self.assertEqual(route.call_args[0][:4], ("cp", "(kicad_pcb demo)", pro, None))
        self.assertEqual([c if isinstance(c, str) else c[0] for c in board.calls],
                         ["begin", "create", "push"])

    def test_errors_become_a_dialog(self):
        board = EditorBoard("")
        kicad = SimpleNamespace(get_board=lambda: board)
        with mock.patch.object(runner, "find_binary",
                               side_effect=runner.RouteError("copperpath binary not found")), \
                mock.patch.object(runner, "load_settings", return_value={}), \
                mock.patch.object(action.ui, "error") as dialog:
            self.assertEqual(action.run(kicad, API), 1)
        self.assertIn("not found", dialog.call_args[0][0])
        self.assertEqual(board.calls, [])

    def test_old_kipy_without_board_text(self):
        with self.assertRaises(runner.RouteError):
            action.board_text(SimpleNamespace())


if __name__ == "__main__":
    unittest.main()
