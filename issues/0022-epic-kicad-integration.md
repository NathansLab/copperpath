# 0022 Epic: copperpath as a fully integrated KiCad tool

GitHub: https://github.com/NathansLab/copperpath/issues/9
Status: open (feature request, epic)

Today copperpath is a CLI that reads and writes `.kicad_pcb` files. To be usable as a real KiCad
router it needs to work inside the running editor, be interactive and be safe to undo.

## Missing pieces (proposed order)

1. **KiCad IPC API plugin (KiCad 9/10)**
   - Python plugin (toolbar button + dialog) that talks to the running PCB editor through the
     official IPC API (`kipy`): read the board from the editor, run `copperpath` as a subprocess,
     push the result back as one commit (one Ctrl+Z undoes the whole route).
   - No file round trip, so no "board changed on disk" dialogs and nothing lost while the editor is open.
   - Package as a PCM (Plugin and Content Manager) package with a prebuilt binary per platform.
2. **Selection-aware commands**
   - Route selected nets / selected footprints / a drawn rectangle (the CLI already has `--nets`
     and `--region`).
   - Rip up selected, keep everything else locked.
   - "Route the rest" after manual work.
3. **Progress and cancel**
   - A machine-readable progress stream (`--progress-json`: iteration, conflict cells,
     nets left, ETA) for a progress bar.
   - Cancel = SIGTERM, which writes the best result so far (#8).
4. **Results inside KiCad**
   - Failed nets and congestion hotspots as DRC-style markers (KiCad IPC can add markers) or as
     a temporary `User.N` layer with circles and labels, so the user sees *where* it failed and why.
5. **Settings from the board**
   - Read every router-relevant setting from the board/project (net classes, DRU rules, rule areas,
     custom rules in text vars). Store copperpath-only settings (yield_pours, power nets, layer
     preferences) in the project file under a `copperpath` key or in a `.kicad_dru` comment block,
     instead of a separate JSON.
6. **Incremental / interactive mode (later)**
   - Long-lived `copperpath serve` process that keeps the model and maps in memory; the plugin
     sends deltas (moved footprint, new lock) and gets a re-route of the affected nets in < 1 s.
   - Basis for "push and shove"-like assisted routing and for placement feedback.
7. **Quality gates before writing**
   - Exact clean check + kicad-cli DRC (exists); refuse to write new errors unless `--allow-drc`.
   - Summary dialog: completion, vias, length, new DRC per type, versus the previous state.

## Acceptance for the first milestone (1-3)
In KiCad 10, a button routes the selected nets of an open board, shows progress, can be cancelled,
and the whole result is undone by one Ctrl+Z.
