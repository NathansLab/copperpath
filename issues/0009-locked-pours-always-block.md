# 0009 Locked copper pours of other nets always block

Status: fixed (unreleased): `--yield-pours PATTERNS` / config key `yield_pours` (zone name
wildcards) lets locked pours yield like unlocked ones

**What was bad**
A locked GND pour covering a row of gate-driver input pins made every stub there illegal
(`wide zone narrow zone`): 58 of 62 nets failed, 215 of 312 pins without a legal stub. KiCad
refills a pour around new tracks, so it need not block.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --cfg cfg.json --region ... --out out.kicad_pcb
```

**Minimal synthetic repro board**
A locked F.Cu zone of net GND named `CELL1_GND` over SMD pads of other nets; unit test
`model::tests::yield_pours_releases_locked_pours`.

**Expected**
An option to let such pours yield; keepout rule areas and fixed copper (tracks, pads) still block.

**Actual**
All stubs under the pour blocked.

**Note**
Unlocked pours never block (they are refilled). A pour of a high-voltage net therefore gets holes
where low-voltage tracks pass; KiCad's refill keeps the clearances. Lock such pours (and do not
list them in `yield_pours`) when tracks must stay out of them.
