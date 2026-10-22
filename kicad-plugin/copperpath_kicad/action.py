"""The plugin actions: editor board (and selection) -> copperpath -> one commit per step."""

import os

from . import delta as deltamod
from . import runner, selection, ui

PLUGIN_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
COMMANDS = ("all", "nets", "region", "ripup", "ripup-route")


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


def route_args(settings):
    """The `args` plugin setting, without options the selection commands set or must not get."""
    args = [str(a) for a in settings.get("args", ())]
    if "--ignore-unlocked" in args:
        # the router would not see unlocked copper, but it stays on the board: overlaps, shorts
        raise runner.RouteError(
            "--ignore-unlocked in the plugin settings: new copper could overlap the existing "
            "unlocked copper; use Rip up selected nets instead"
        )
    for opt in ("--nets", "--region"):
        if opt in args:
            raise runner.RouteError(f"{opt} in the plugin settings: the commands set it")
    return args


def command_args(settings, nets=None, region=None):
    """`route` arguments: the plugin setting plus the selection."""
    args = route_args(settings)
    if nets is not None:
        args += ["--nets", selection.nets_arg(nets)]
    if region is not None:
        args += ["--region", selection.region_arg(region)]
    return args


def route(board, binary, args, api, nets=None):
    """Route with `args` (progress dialog, cancel) and apply the delta as one commit;
    (removed, added). Raises `runner.Discarded` if the user throws the result away."""
    pro, dru = project_files(board)
    text = board_text(board)
    delta = runner.route(binary, text, pro, dru, args, watch=ui.progress_dialog)
    deltamod.check_scope(delta, nets)
    # the editor stays usable during the route: new copper drawn meanwhile is not in the result
    if board_text(board) != text and not ui.ask(
        "copperpath",
        "The board changed while copperpath was running. The new route does not know these "
        "changes and can overlap them.\n\nApply the route anyway? (One Ctrl+Z undoes it.)",
    ):
        raise runner.Discarded()
    return deltamod.apply(board, delta, api)


def rip_up(board, nets):
    """Remove the unlocked copper of `nets` in its own commit; (removed, locked kept)."""
    items, locked = selection.ripup_items(board, nets)
    removed, _ = deltamod.commit_items(board, items, [], "copperpath rip up")
    return removed, locked


def run(kicad=None, api=None, command="all"):
    """Run one command on the open board; returns a process exit code.

    `all`: route every unrouted connection (existing copper stays as it is),
    `nets`: route the selected nets, `region`: route inside the selected shape or zone,
    `ripup`: remove the unlocked copper of the selected nets, `ripup-route`: rip up, then route
    those nets again (two commits, so Ctrl+Z first undoes the route, then the rip-up).
    """
    if command not in COMMANDS:
        raise ValueError(f"unknown command {command!r}")
    try:
        if kicad is None:
            from kipy import KiCad

            kicad = KiCad()
        board = kicad.get_board()
        api = api or deltamod.kipy_api()
        nets = region = None
        if command != "all":
            items = list(board.get_selection())
            if command == "region":
                region = selection.selected_region(board, items)
            else:
                nets = selection.selected_nets(items)
        if command != "ripup":
            # settings, binary and arguments are checked before the rip-up changes anything
            settings = runner.load_settings(PLUGIN_DIR)
            binary = runner.find_binary(PLUGIN_DIR, settings)
            args = command_args(settings, nets, region)
        if command.startswith("ripup"):
            removed, locked = rip_up(board, nets)
            print(f"copperpath: ripped up {removed} items, kept {locked} locked")
            if command == "ripup":
                return 0
        removed, added = route(board, binary, args, api, nets)
    except runner.Discarded:
        print("copperpath: route discarded, the board is unchanged by it")
        return 0
    except runner.RouteError as e:
        ui.error(str(e), e.log)
        return 1
    except (deltamod.DeltaError, selection.SelectionError) as e:
        ui.error(str(e))
        return 1
    except Exception as e:  # editor not reachable, API errors: show instead of dying silently
        ui.error(f"{type(e).__name__}: {e}")
        return 1
    print(f"copperpath: removed {removed}, added {added} items")
    return 0
