"""The "Route with copperpath" action: editor board -> copperpath -> one commit."""

import os

from . import delta as deltamod
from . import runner, ui

PLUGIN_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def board_text(board):
    """The board as KiCad writes it, including unsaved edits (no file round trip)."""
    get = getattr(board, "get_as_string", None)
    if get is None:
        raise runner.RouteError(
            "this kicad-python version has no Board.get_as_string(); "
            "update KiCad (9.0.1 or newer) and kicad-python"
        )
    return get()


def project_files(board):
    """(.kicad_pro, .kicad_dru) next to the board in the editor, None where missing."""
    try:
        folder = board.get_project().path
        stem = os.path.splitext(os.path.basename(board.name))[0]
    except Exception:
        return None, None
    if not folder or not stem:
        return None, None
    found = []
    for ext in (".kicad_pro", ".kicad_dru"):
        p = os.path.join(folder, stem + ext)
        found.append(p if os.path.isfile(p) else None)
    return tuple(found)


def run(kicad=None, api=None):
    """Route the open board; returns a process exit code."""
    try:
        if kicad is None:
            from kipy import KiCad

            kicad = KiCad()
        board = kicad.get_board()
        settings = runner.load_settings(PLUGIN_DIR)
        binary = runner.find_binary(PLUGIN_DIR, settings)
        pro, dru = project_files(board)
        delta = runner.route(binary, board_text(board), pro, dru, settings.get("args", ()))
        removed, added = deltamod.apply(board, delta, api or deltamod.kipy_api())
    except runner.RouteError as e:
        ui.error(str(e), e.log)
        return 1
    except deltamod.DeltaError as e:
        ui.error(str(e))
        return 1
    except Exception as e:  # editor not reachable, API errors: show instead of dying silently
        ui.error(f"{type(e).__name__}: {e}")
        return 1
    print(f"copperpath: removed {removed}, added {added} items")
    return 0
