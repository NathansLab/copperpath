"""Find the copperpath binary and run `copperpath route` on board text from the editor."""

import json
import os
import shutil
import subprocess
import sys
import tempfile

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


def build_command(binary, delta_path, pro=None, dru=None, extra_args=()):
    """`copperpath route` reading the board from stdin and writing only the delta."""
    cmd = [binary, "route", "--pcb", "-", "--delta-json", delta_path]
    if pro:
        cmd += ["--pro", pro]
    if dru:
        cmd += ["--dru", dru]
    return cmd + [str(a) for a in extra_args]


def tail(text, n=LOG_LINES):
    return "\n".join(text.strip().splitlines()[-n:])


def route(binary, board_text, pro=None, dru=None, extra_args=(), run=subprocess.run):
    """Route the board text and return the parsed delta."""
    with tempfile.TemporaryDirectory(prefix="copperpath-") as tmp:
        delta_path = os.path.join(tmp, "delta.json")
        cmd = build_command(binary, delta_path, pro, dru, extra_args)
        kw = {}
        if sys.platform == "win32":
            # no console window flashing up over the editor
            kw["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0)
        try:
            p = run(
                cmd,
                input=board_text.encode("utf-8"),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                cwd=tmp,
                **kw,
            )
        except OSError as e:
            raise RouteError(f"cannot start {binary}: {e}") from e
        log = (p.stderr or b"").decode("utf-8", "replace") + (p.stdout or b"").decode(
            "utf-8", "replace"
        )
        if p.returncode != 0:
            raise RouteError(f"copperpath exited with code {p.returncode}", tail(log))
        try:
            with open(delta_path, encoding="utf-8") as f:
                text = f.read()
        except OSError as e:
            raise RouteError("copperpath wrote no delta file", tail(log)) from e
    try:
        return deltamod.load(text)
    except deltamod.DeltaError as e:
        raise RouteError(str(e), tail(log)) from e
