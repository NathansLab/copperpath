# copperpath KiCad plugin

A KiCad 9/10 plugin on the official IPC API ([kicad-python](https://pypi.org/project/kicad-python/),
`kipy`). It adds a toolbar button **Route with copperpath** to the PCB editor:

1. reads the open board from the editor (`Board.get_as_string()`, unsaved edits included),
2. runs `copperpath route --pcb - --delta-json …` with the board text on stdin and the
   `.kicad_pro` / `.kicad_dru` next to the board,
3. applies the [delta](../README.md#change-set-for-the-kicad-ipc-api---delta-json) as **one**
   commit (`begin_commit`, `remove_items` / `create_items`, `push_commit("copperpath route")`).

No file is written or reloaded, so there is no "board changed on disk" dialog, and one Ctrl+Z
restores the board as it was. Errors (binary missing, exit code ≠ 0, a delta that does not fit
the board) are shown in a dialog with the last log lines; nothing is changed then.

Part of [#9](https://github.com/NathansLab/copperpath/issues/9); progress/cancel and a PCM
package with a bundled binary follow.

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

Needs KiCad 9.0.1 or newer with the IPC API, and copperpath with `--delta-json` / `--pcb -`.

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

The delta → kipy conversion, the commit flow, the binary lookup and the subprocess handling are
tested against a mocked kipy API; neither KiCad nor kipy is needed:

```sh
python -m pytest kicad-plugin/tests
```

Manual check in KiCad 10 on the demo board (`examples/demo/demo.kicad_pcb`): open the board, press the
button, the board is routed; one Ctrl+Z removes all new tracks and vias. Then select a pad each of
two nets and press **Route selected nets**: only those two nets get copper. Draw a track by hand
and press **Route with copperpath** again: the track stays as drawn.
