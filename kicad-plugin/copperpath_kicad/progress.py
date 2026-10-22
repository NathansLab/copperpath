"""What the progress dialog shows, computed from copperpath's `--progress-json` events.

No GUI code here, so it can be tested without tkinter or KiCad.
"""

# phase -> (label, start of its share of the progress bar)
PHASES = {
    "load": ("Loading the board", 0.0),
    "droute": ("Routing (negotiation)", 0.05),
    "resolve": ("Rip-up and reroute of the remaining conflicts", 0.85),
    "fallback": ("Fallback routes without cleanup", 0.85),
    "clean": ("Cleanup and exact check", 0.9),
    "write": ("Writing the result", 0.95),
    "check": ("Counting opens", 0.97),
}
# share of the bar for the negotiation iterations
DROUTE_SPAN = PHASES["resolve"][1] - PHASES["droute"][1]


def duration(s):
    """`42 s`, `3 min 05 s`, `1 h 02 min`; `–` for None."""
    if s is None:
        return "–"
    s = int(round(s))
    if s < 60:
        return f"{s} s"
    if s < 3600:
        return f"{s // 60} min {s % 60:02d} s"
    return f"{s // 3600} h {s % 3600 // 60:02d} min"


class ProgressState:
    """Feed it events with `update()`; read `phase`, `rows()`, `fraction` and `done`."""

    def __init__(self):
        self.phase = None
        self.phase_t = 0.0
        self.t = 0.0
        self.iter = None
        self.done = None
        self.fraction = 0.0

    def update(self, ev):
        kind = ev.get("ev")
        self.t = max(self.t, float(ev.get("t") or 0.0))
        if kind == "phase":
            self.phase = ev.get("phase")
            self.phase_t = self.t
            self._advance(PHASES.get(self.phase, ("", self.fraction))[1])
        elif kind == "iter":
            self.iter = ev
            self._advance(self._iter_fraction(ev))
        elif kind == "done":
            self.done = ev
            if ev.get("ok"):
                self._advance(1.0)

    def _iter_fraction(self, ev):
        """Share of the negotiation done: elapsed / (elapsed + ETA), else iterations / maximum."""
        eta = ev.get("eta_s")
        spent = self.t - self.phase_t if self.phase == "droute" else 0.0
        if eta is not None and spent + eta > 0:
            x = spent / (spent + eta)
        else:
            x = (ev.get("iter", 0) + 1) / max(1, ev.get("iters_max") or 1)
        return PHASES["droute"][1] + DROUTE_SPAN * min(1.0, x)

    def _advance(self, f):
        # never runs backwards (a new ETA can be longer than the previous one)
        self.fraction = max(self.fraction, min(1.0, f))

    def phase_label(self):
        if self.done is not None:
            return "Finished" if self.done.get("ok") else "Failed"
        if self.phase is None:
            return "Starting copperpath"
        return PHASES.get(self.phase, (self.phase, 0))[0]

    def rows(self, elapsed=None):
        """(label, value) pairs for the dialog; `elapsed`: wall-clock seconds (events can be
        minutes apart on a big board), else the time of the last event."""
        it = self.iter or {}
        eta = it.get("eta_s") if self.phase == "droute" else None
        nets = (f"{it['nets_left']} of {it['nets_total']}"
                if "nets_left" in it and "nets_total" in it else "–")
        return [
            ("Phase", self.phase_label()),
            ("Iteration", f"{it['iter'] + 1} of at most {it.get('iters_max', '?')}"
             if "iter" in it else "–"),
            ("Conflict cells", str(it.get("conflict_cells", "–"))),
            ("Nets left", nets),
            ("Elapsed", duration(max(self.t, elapsed or 0.0))),
            ("Remaining (negotiation)", duration(eta)),
        ]

    def summary(self):
        """One line about the finished run (None while it runs)."""
        d = self.done
        if d is None:
            return None
        if not d.get("ok"):
            return f"copperpath failed: {d.get('error', 'unknown error')}"
        s = f"{d.get('routed', 0)} nets routed, {d.get('failed', 0)} failed"
        if d.get("stopped"):
            s += f" (stopped early: {d['stopped']}, best result so far)"
        return s
