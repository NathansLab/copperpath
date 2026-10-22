# copperpath KiCad plugin

A KiCad 9/10 plugin on the official IPC API ([kicad-python](https://pypi.org/project/kicad-python/),
`kipy`). It adds a toolbar button **Route with copperpath** to the PCB editor:

1. reads the open board from the editor (`Board.get_as_string()`, unsaved edits included),
2. runs `copperpath route --pcb <temp file> --delta-json … --progress-json --cancel-file …
   --cancel-on-stdin-eof` with the `.kicad_pro` / `.kicad_dru` next to the board, and shows a
   progress dialog while it runs,
3. applies the [delta](../README.md#change-set-for-the-kicad-ipc-api---delta-json) as **one**
   commit (`begin_commit`, `remove_items` / `create_items`, `push_commit("copperpath route")`).

The board's own file is never written or reloaded, so there is no "board changed on disk"
dialog, and one Ctrl+Z restores the board as it was. Errors (binary missing, exit code ≠ 0, a
delta that does not fit the board) are shown in a dialog with the last log lines; nothing is
changed then.

Part of [#9](https://github.com/NathansLab/copperpath/issues/9); a PCM package with a bundled
binary follows.

## Progress and cancel

While copperpath runs, a small window (tkinter, always on top) shows the phase, the negotiation
iteration, the conflict cells, the nets left, a progress bar, the elapsed time and an ETA. The
data are the [`--progress-json`](../README.md#progress-events---progress-json) events, read line
by line in a thread. The plugin is its own process, so **the editor stays usable** meanwhile.

| button | what happens |
|---|---|
| **Stop and apply best result** | creates the cancel file and closes copperpath's stdin; copperpath ends the current step, writes the best result so far, and the plugin applies it as one commit (one Ctrl+Z) |
| **Cancel and discard** (or closing the window) | ends copperpath at once; the board is not changed by the route |

- If the board changed while copperpath was running (any difference in the board text), the
  plugin asks before applying: the route does not know the changes and can overlap them.
- The board goes to copperpath as a temporary file, not on stdin, so stdin stays open: if the
  plugin or KiCad ends, copperpath sees stdin close and stops instead of running on.
- The ETA covers the negotiation (the longest part) and is capped by a `--time-limit` from the
  settings; the negotiation often ends earlier on a plateau.
- Without tkinter there is no window: the plugin waits, prints the progress to its log, and a
  changed board is not applied.
- *Rip up and reroute*: discarding the route keeps the rip-up commit (Ctrl+Z restores it).

## Actions

| action | button | what it does |
|---|---|---|
| **Route with copperpath** | yes | routes every unrouted connection ("route the rest") |
| **Route selected nets** | yes | routes the nets of the selected tracks, vias, pads and footprints (`--nets`) |
| **Route region** | yes | routes the nets whose pads lie in the bounding box of the selected shapes or zones (`--region`); draw a rectangle on a User layer and select it |
| **Rip up selected nets** | menu | removes the unlocked tracks and vias of the selected nets (locked ones stay) |
| **Rip up and reroute selected nets** | menu | the rip-up, then *Route selected nets* |

- Existing copper is never changed by a route: every track and via keeps its UUID, manual work
  after a route stays as drawn, and a route only adds copper. The plugin refuses a delta that
  removes items or, for the selection commands, adds copper to a net outside the selection.
- Each route is one commit ("copperpath route"), each rip-up another ("copperpath rip up"):
  after *Rip up and reroute*, the first Ctrl+Z removes the new route, the second brings the old
  copper back.
- Settings, binary and arguments are checked before the rip-up, so a failing *Rip up and
  reroute* changes nothing. If the route itself fails, the rip-up stays (Ctrl+Z restores it).
- Nothing selected for a selection command: a dialog says what to select, nothing is changed.

## Install

Needs KiCad 9.0.1 or newer with the IPC API, and copperpath with `--delta-json` and
`--progress-json`.

1. In KiCad: *Preferences → Plugins → Enable KiCad API*. KiCad creates a Python environment for
   the plugin and installs `requirements.txt` on first use.
2. Copy (or symlink) this `kicad-plugin` folder into KiCad's plugin folder, e.g. as
   `copperpath`:
   - Windows: `%USERPROFILE%\Documents\KiCad\<version>\plugins\`
   - Linux: `~/.local/share/kicad/<version>/plugins/`
   - macOS: `~/Documents/KiCad/<version>/plugins/`
3. Restart the PCB editor; the button appears in the top toolbar (and under
   *Tools → External Plugins*).

## Finding the copperpath binary

In this order:

1. `binary` in `settings.json` in the plugin folder,
2. the bundled binary `bin/copperpath` (`bin/copperpath.exe` on Windows),
3. `copperpath` on `PATH`.

`settings.json` (optional):

```json
{"binary": "C:/tools/copperpath.exe", "args": ["--time-limit", "120"]}
```

`args` are appended to the `route` command (any [route option](../README.md#route-options)
except `--nets` and `--region`, which the actions set, and `--ignore-unlocked`, which would let
new copper overlap the existing unlocked copper).
copperpath reads the rules from the project files on disk, so save Board Setup changes before
routing.

## Tests

The delta → kipy conversion, the commit flow, the binary lookup, the subprocess handling
(progress events, cancel, discard) and the progress display are tested against a mocked kipy
API and a fake process; neither KiCad, kipy nor copperpath is needed:

```sh
python -m pytest kicad-plugin/tests
```

Manual check in KiCad 10 on the demo board (`examples/demo/demo.kicad_pcb`): open the board, press the
button, the board is routed; one Ctrl+Z removes all new tracks and vias. Then select a pad each of
two nets and press **Route selected nets**: only those two nets get copper. Draw a track by hand
and press **Route with copperpath** again: the track stays as drawn. On a bigger board, press
**Stop and apply best result** during the negotiation: the partial result arrives as one commit;
press the button again and **Cancel and discard**: nothing changes.
