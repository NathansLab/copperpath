"""Find the copperpath binary and run `copperpath route` on board text from the editor."""

import collections
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading

from . import delta as deltamod

EXE = "copperpath.exe" if sys.platform == "win32" else "copperpath"
SETTINGS = "settings.json"
LOG_LINES = 20


class RouteError(Exception):
    """copperpath could not be run or failed; `log` holds the last lines of its output."""

    def __init__(self, message, log=""):
        super().__init__(message)
        self.log = log


def load_settings(plugin_dir):
    """Plugin settings from `settings.json` next to the plugin ({} if there is none).

    Keys: `binary` (path to copperpath), `args` (extra `route` arguments, e.g.
    ["--time-limit", "120"]).
    """
    path = os.path.join(plugin_dir, SETTINGS)
    if not os.path.isfile(path):
        return {}
    try:
        with open(path, encoding="utf-8") as f:
            s = json.load(f)
    except (OSError, ValueError) as e:
        raise RouteError(f"cannot read plugin settings {path}: {e}") from e
    if not isinstance(s, dict):
        raise RouteError(f"plugin settings {path} must be a JSON object")
    return s


def find_binary(plugin_dir, settings, which=shutil.which):
    """The plugin setting `binary`, then the binary bundled in `bin/`, then `PATH`."""
    configured = settings.get("binary")
    if configured:
        configured = os.path.expanduser(configured)
        if not os.path.isfile(configured):
            raise RouteError(
                f"copperpath binary from the plugin setting not found: {configured}"
            )
        return configured
    bundled = os.path.join(plugin_dir, "bin", EXE)
    if os.path.isfile(bundled):
        return bundled
    on_path = which("copperpath")
    if on_path:
        return on_path
    raise RouteError(
        "copperpath binary not found: set `binary` in "
        f"{os.path.join(plugin_dir, SETTINGS)}, put it into {os.path.dirname(bundled)} "
        "or on PATH"
    )


class Discarded(Exception):
    """The user cancelled the route and discarded its result; the board is unchanged."""


def build_command(binary, pcb_path, delta_path, cancel_path, pro=None, dru=None, extra_args=()):
    """`copperpath route` writing only the delta, with the progress stream and both cancel paths.

    The board goes in as a file, not on stdin: stdin stays open, so the router stops (and writes
    its best result) when the plugin closes it or dies (`--cancel-on-stdin-eof`).
    """
    cmd = [binary, "route", "--pcb", pcb_path, "--delta-json", delta_path,
           "--progress-json", "--cancel-file", cancel_path, "--cancel-on-stdin-eof"]
    if pro:
        cmd += ["--pro", pro]
    if dru:
        cmd += ["--dru", dru]
    return cmd + [str(a) for a in extra_args]


def tail(text, n=LOG_LINES):
    return "\n".join(text.strip().splitlines()[-n:])


class RouteJob:
    """A running `copperpath route`: progress events from stderr (read in a thread), cancel, kill.

    `start()`, then poll `events()` / `finished()`, then `result()` for the delta. `cancel()` stops
    the router, which still writes its best result; `kill()` ends it, there is no result then.
    """

    def __init__(self, binary, board_text, pro=None, dru=None, extra_args=(),
                 popen=subprocess.Popen):
        self.binary = binary
        self.board_text = board_text
        self.pro, self.dru, self.extra_args = pro, dru, extra_args
        self.popen = popen
        self.proc = None
        self.tmp = None
        self.log = collections.deque(maxlen=200)
        self.queue = queue.Queue()
        self.done = None
        self.cancelled = False
        self.reader = None

    def start(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="copperpath-")
        d = self.tmp.name
        pcb = os.path.join(d, "board.kicad_pcb")
        with open(pcb, "w", encoding="utf-8", newline="") as f:
            f.write(self.board_text)
        self.delta_path = os.path.join(d, "delta.json")
        self.cancel_path = os.path.join(d, "cancel")
        cmd = build_command(self.binary, pcb, self.delta_path, self.cancel_path, self.pro,
                            self.dru, self.extra_args)
        kw = {}
        if sys.platform == "win32":
            # no console window flashing up over the editor
            kw["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0)
        try:
            self.proc = self.popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.PIPE, cwd=d, **kw)
        except OSError as e:
            self.cleanup()
            raise RouteError(f"cannot start {self.binary}: {e}") from e
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        return self

    def _read(self):
        for raw in self.proc.stderr:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            ev = parse_event(line)
            if ev is None:
                self.log.append(line)
            else:
                if ev.get("ev") == "done":
                    self.done = ev
                self.queue.put(ev)

    def events(self):
        """The progress events that arrived since the last call."""
        out = []
        while True:
            try:
                out.append(self.queue.get_nowait())
            except queue.Empty:
                return out

    def finished(self):
        return self.proc.poll() is not None

    def cancel(self):
        """Stop like `--time-limit`: the router writes the best result so far."""
        self.cancelled = True
        try:
            with open(self.cancel_path, "w", encoding="utf-8"):
                pass
        except OSError:
            pass
        try:
            self.proc.stdin.close()
        except OSError:
            pass

    def kill(self):
        """End the router at once; there is no result."""
        self.cancelled = True
        try:
            self.proc.kill()
        except OSError:
            pass
        self.proc.wait()
        self.cleanup()

    def wait(self):
        self.proc.wait()
        if self.reader is not None:
            self.reader.join()

    def result(self):
        """Wait for the end and return the parsed delta."""
        try:
            self.wait()
            log = "\n".join(self.log)
            if self.proc.returncode != 0:
                msg = (self.done or {}).get("error")
                raise RouteError(
                    f"copperpath exited with code {self.proc.returncode}"
                    + (f": {msg}" if msg else ""), tail(log))
            try:
                with open(self.delta_path, encoding="utf-8") as f:
                    text = f.read()
            except OSError as e:
                raise RouteError("copperpath wrote no delta file", tail(log)) from e
        finally:
            self.cleanup()
        try:
            return deltamod.load(text)
        except deltamod.DeltaError as e:
            raise RouteError(str(e), tail(log)) from e

    def cleanup(self):
        if self.proc is not None and self.proc.stdin and not self.proc.stdin.closed:
            try:
                self.proc.stdin.close()
            except OSError:
                pass
        if self.tmp is not None:
            self.tmp.cleanup()
            self.tmp = None


def parse_event(line):
    """A `--progress-json` event (dict) from a stderr line, None for a normal log line."""
    if not line.startswith("{"):
        return None
    try:
        ev = json.loads(line)
    except ValueError:
        return None
    return ev if isinstance(ev, dict) and "ev" in ev else None


def route(binary, board_text, pro=None, dru=None, extra_args=(), watch=None,
          popen=subprocess.Popen):
    """Route the board text and return the parsed delta.

    `watch(job)` (the progress dialog) runs while copperpath works and returns "apply" or
    "discard"; without it the call just waits. "discard" raises `Discarded`.
    """
    job = RouteJob(binary, board_text, pro, dru, extra_args, popen).start()
    if watch is not None:
        try:
            choice = watch(job)
        except BaseException:
            job.kill()
            raise
        if choice == "discard":
            job.kill()
            raise Discarded()
    return job.result()
