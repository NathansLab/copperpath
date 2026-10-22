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

from copperpath_kicad import action, progress, runner  # noqa: E402

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


EVENTS = [
    {"ev": "phase", "phase": "load", "t": 0.0},
    {"ev": "phase", "phase": "droute", "t": 0.5},
    {"ev": "iter", "iter": 0, "conflict_cells": 40, "nets_conflicted": 3, "nets_left": 4,
     "nets_total": 10, "iters_max": 25, "eta_s": 6.0, "t": 2.5},
    {"ev": "done", "v": 1, "ok": True, "routed": 10, "failed": 0, "t": 3.0},
]


class FakeStdin:
    def __init__(self):
        self.closed = False

    def close(self):
        self.closed = True


class FakeProc:
    """`subprocess.Popen` stand-in: writes the delta and the stderr lines given."""

    def __init__(self, cmd, code=0, delta=DELTA, err=b"", **kw):
        self.cmd, self.kw = cmd, kw
        with open(cmd[cmd.index("--pcb") + 1], encoding="utf-8") as f:
            self.pcb_text = f.read()
        if delta is not None:
            with open(cmd[cmd.index("--delta-json") + 1], "w", encoding="utf-8") as f:
                json.dump(delta, f)
        self.stdin = FakeStdin()
        self.stderr = iter(err.splitlines(keepends=True))
        self.code = code
        self.returncode = None
        self.killed = False

    def poll(self):
        return self.returncode

    def wait(self):
        if self.returncode is None:
            self.returncode = self.code
        return self.returncode

    def kill(self):
        self.killed = True
        self.returncode = -9


def ndjson(events, log=b"[route] done\n"):
    return log + b"".join(json.dumps(e).encode() + b"\n" for e in events)


class RouteTest(unittest.TestCase):
    def test_command(self):
        cmd = runner.build_command("cp", "b.kicad_pcb", "d.json", "stop", "b.kicad_pro", None,
                                   ["--time-limit", 60])
        self.assertEqual(cmd, ["cp", "route", "--pcb", "b.kicad_pcb", "--delta-json", "d.json",
                               "--progress-json", "--cancel-file", "stop",
                               "--cancel-on-stdin-eof", "--pro", "b.kicad_pro",
                               "--time-limit", "60"])

    def fake_popen(self, **kw):
        seen = {}

        def popen(cmd, **pkw):
            seen["proc"] = FakeProc(cmd, **kw, **pkw)
            return seen["proc"]

        return popen, seen

    def test_board_text_in_a_file_and_delta_parsed(self):
        popen, seen = self.fake_popen(err=ndjson(EVENTS))
        delta = runner.route("cp", "(kicad_pcb)", popen=popen)
        p = seen["proc"]
        self.assertEqual(p.pcb_text, "(kicad_pcb)")
        self.assertEqual(p.cmd[:2], ["cp", "route"])
        self.assertIn("--progress-json", p.cmd)
        self.assertEqual(len(delta["add"]), 3)
        # stdin stays open while it runs (cancel on EOF), closed at the end
        self.assertTrue(p.stdin.closed)
        # the temporary folder is gone
        self.assertFalse(os.path.exists(p.cmd[p.cmd.index("--pcb") + 1]))

    def test_events_and_log_are_split(self):
        popen, _ = self.fake_popen(err=ndjson(EVENTS) + b"{not json\n")
        job = runner.RouteJob("cp", "", popen=popen).start()
        job.wait()
        self.assertEqual([e["ev"] for e in job.events()], ["phase", "phase", "iter", "done"])
        self.assertEqual(job.events(), [])
        self.assertEqual(list(job.log), ["[route] done", "{not json"])
        self.assertTrue(job.done["ok"])
        job.result()

    def test_watch_sees_the_events_and_cancel_keeps_the_result(self):
        popen, seen = self.fake_popen(err=ndjson(EVENTS))
        got = []

        def watch(job):
            job.wait()
            got.extend(job.events())
            job.cancel()
            self.assertTrue(os.path.exists(job.cancel_path))
            return "apply"

        delta = runner.route("cp", "", popen=popen, watch=watch)
        self.assertEqual(len(got), 4)
        self.assertTrue(seen["proc"].stdin.closed)
        self.assertEqual(len(delta["add"]), 3)

    def test_discard_kills_and_raises(self):
        popen, seen = self.fake_popen()
        with self.assertRaises(runner.Discarded):
            runner.route("cp", "", popen=popen, watch=lambda job: "discard")
        self.assertTrue(seen["proc"].killed)

    def test_nonzero_exit_reports_log_tail(self):
        log = "".join(f"line {i}\n" for i in range(50)).encode()
        done = {"ev": "done", "v": 1, "ok": False, "error": "board file not found"}
        popen, _ = self.fake_popen(code=2, delta=None, err=ndjson([done], log))
        with self.assertRaises(runner.RouteError) as e:
            runner.route("cp", "", popen=popen)
        self.assertIn("code 2: board file not found", str(e.exception))
        self.assertEqual(e.exception.log.splitlines()[-1], "line 49")
        self.assertEqual(len(e.exception.log.splitlines()), runner.LOG_LINES)

    def test_no_or_bad_delta(self):
        for delta in (None, {"v": 99, "units": "nm"}):
            popen, _ = self.fake_popen(delta=delta)
            with self.assertRaises(runner.RouteError):
                runner.route("cp", "", popen=popen)

    def test_binary_cannot_start(self):
        def popen(cmd, **kw):
            raise FileNotFoundError(cmd[0])

        with self.assertRaises(runner.RouteError):
            runner.route("cp", "", popen=popen)

    def test_parse_event(self):
        self.assertEqual(runner.parse_event('{"ev":"phase","phase":"load","t":0}')["phase"],
                         "load")
        for line in ("[route] x", "{broken", '{"no_ev": 1}', "[1, 2]"):
            self.assertIsNone(runner.parse_event(line))


class ProgressStateTest(unittest.TestCase):
    def test_rows_and_fraction(self):
        s = progress.ProgressState()
        self.assertEqual(s.phase_label(), "Starting copperpath")
        seen = []
        for ev in EVENTS[:3]:
            s.update(ev)
            seen.append(s.fraction)
        rows = dict(s.rows())
        self.assertEqual(rows["Phase"], "Routing (negotiation)")
        self.assertEqual(rows["Iteration"], "1 of at most 25")
        self.assertEqual(rows["Conflict cells"], "40")
        self.assertEqual(rows["Nets left"], "4 of 10")
        self.assertEqual(rows["Remaining (negotiation)"], "6 s")
        self.assertEqual(dict(s.rows(elapsed=75))["Elapsed"], "1 min 15 s")
        # 2 s of negotiation, 6 s to go: a quarter of the negotiation share
        self.assertAlmostEqual(seen[-1], 0.05 + 0.8 * 0.25)
        self.assertEqual(seen, sorted(seen))
        # a longer ETA does not move the bar back
        s.update(dict(EVENTS[2], iter=1, eta_s=600.0, t=3.0))
        self.assertAlmostEqual(s.fraction, seen[-1])
        s.update(EVENTS[3])
        self.assertEqual((s.fraction, s.phase_label()), (1.0, "Finished"))
        self.assertEqual(s.summary(), "10 nets routed, 0 failed")

    def test_no_eta_uses_the_iteration_count(self):
        s = progress.ProgressState()
        s.update({"ev": "phase", "phase": "droute", "t": 0})
        s.update({"ev": "iter", "iter": 4, "iters_max": 10, "eta_s": None, "t": 1})
        self.assertAlmostEqual(s.fraction, 0.05 + 0.8 * 0.5)
        self.assertEqual(dict(s.rows())["Remaining (negotiation)"], "–")

    def test_stopped_and_failed_summary(self):
        s = progress.ProgressState()
        s.update({"ev": "done", "ok": True, "routed": 8, "failed": 2, "stopped": "cancel"})
        self.assertIn("stopped early: cancel", s.summary())
        s = progress.ProgressState()
        s.update({"ev": "done", "ok": False, "error": "boom"})
        self.assertEqual((s.phase_label(), s.summary()), ("Failed", "copperpath failed: boom"))
        self.assertLess(s.fraction, 1.0)

    def test_duration(self):
        self.assertEqual([progress.duration(x) for x in (None, 4.4, 65, 3725)],
                         ["–", "4 s", "1 min 05 s", "1 h 02 min"])


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

    def run_action(self, board, **route):
        kicad = SimpleNamespace(get_board=lambda: board)
        with mock.patch.object(runner, "find_binary", return_value="cp"), \
                mock.patch.object(runner, "route", **route) as r, \
                mock.patch.object(runner, "load_settings", return_value={}):
            code = action.run(kicad, API)
        self.assertIs(r.call_args[1]["watch"], action.ui.progress_dialog)
        return code

    def test_discarded_route_changes_nothing(self):
        board = EditorBoard("")
        self.assertEqual(self.run_action(board, side_effect=runner.Discarded()), 0)
        self.assertEqual(board.calls, [])

    def test_board_changed_during_the_route_asks_first(self):
        for answer, calls in ((False, []), (True, ["begin", "create", "push"])):
            board = EditorBoard("")
            texts = iter(["(kicad_pcb demo)", "(kicad_pcb demo edited)"])
            board.get_as_string = lambda: next(texts)
            with mock.patch.object(action.ui, "ask", return_value=answer) as ask:
                self.assertEqual(self.run_action(board, return_value=DELTA), 0)
            self.assertIn("changed", ask.call_args[0][1])
            self.assertEqual([c if isinstance(c, str) else c[0] for c in board.calls], calls)

    def test_unchanged_board_is_applied_without_asking(self):
        board = EditorBoard("")
        with mock.patch.object(action.ui, "ask") as ask:
            self.assertEqual(self.run_action(board, return_value=DELTA), 0)
        ask.assert_not_called()

    def test_old_kipy_without_board_text(self):
        with self.assertRaises(runner.RouteError):
            action.board_text(SimpleNamespace())


if __name__ == "__main__":
    unittest.main()
