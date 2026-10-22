"""Minimal dialogs. IPC plugins run as their own process, so tkinter is used and the editor stays
usable while a dialog is open; without tkinter the message goes to stderr (shown in KiCad's API
log)."""

import sys
import time

from . import progress

POLL_MS = 200


def message(title, text, error=False):
    print(f"{title}: {text}", file=sys.stderr if error else sys.stdout)
    try:
        import tkinter
        from tkinter import messagebox
    except ImportError:
        return
    try:
        root = tkinter.Tk()
        root.withdraw()
        root.attributes("-topmost", True)
        (messagebox.showerror if error else messagebox.showinfo)(title, text, parent=root)
        root.destroy()
    except tkinter.TclError:
        pass


def error(text, log=""):
    if log:
        text = f"{text}\n\nLast log lines:\n{log}"
    message("copperpath", text, error=True)


def ask(title, text):
    """Yes/no question; False without tkinter (the safe answer: nothing is changed)."""
    print(f"{title}: {text}", file=sys.stderr)
    try:
        import tkinter
        from tkinter import messagebox
    except ImportError:
        return False
    try:
        root = tkinter.Tk()
        root.withdraw()
        root.attributes("-topmost", True)
        yes = messagebox.askyesno(title, text, icon="warning", parent=root)
        root.destroy()
        return bool(yes)
    except tkinter.TclError:
        return False


def wait_quietly(job, state):
    """No GUI: wait for the router, progress lines go to stdout."""
    while not job.finished():
        for ev in job.events():
            state.update(ev)
            if ev.get("ev") == "iter":
                print("copperpath: " + ", ".join(f"{k} {v}" for k, v in state.rows()))
        time.sleep(POLL_MS / 1000)
    return "apply"


def progress_dialog(job, title="copperpath route"):
    """Progress of a running `runner.RouteJob` until it ends; returns "apply" or "discard".

    *Stop and apply best result* cancels the router, which then writes its best result; the
    dialog closes when it has. *Cancel and discard* (or closing the window) returns "discard" at
    once.
    """
    state = progress.ProgressState()
    t0 = time.monotonic()
    try:
        import tkinter
        from tkinter import ttk
    except ImportError:
        return wait_quietly(job, state)
    try:
        root = tkinter.Tk()
    except tkinter.TclError:
        return wait_quietly(job, state)
    root.title(title)
    root.attributes("-topmost", True)
    root.resizable(False, False)
    frame = ttk.Frame(root, padding=12)
    frame.grid(sticky="nsew")
    values = []
    for i, (label, value) in enumerate(state.rows()):
        ttk.Label(frame, text=label + ":").grid(row=i, column=0, sticky="w", padx=(0, 12))
        v = tkinter.StringVar(value=value)
        ttk.Label(frame, textvariable=v, width=42).grid(row=i, column=1, sticky="w")
        values.append(v)
    n = len(values)
    bar = ttk.Progressbar(frame, length=420, maximum=1000, mode="determinate")
    bar.grid(row=n, column=0, columnspan=2, pady=(10, 4), sticky="ew")
    status = tkinter.StringVar(value="The editor stays usable while copperpath runs.")
    ttk.Label(frame, textvariable=status, wraplength=420).grid(
        row=n + 1, column=0, columnspan=2, sticky="w")
    buttons = ttk.Frame(frame)
    buttons.grid(row=n + 2, column=0, columnspan=2, pady=(10, 0), sticky="e")
    choice = {"v": "apply"}

    def stop():
        job.cancel()
        stop_btn.configure(state="disabled", text="Stopping…")
        status.set("Stopping: copperpath ends the current step and keeps the best result so "
                   "far, which is then applied as one commit.")

    def discard():
        choice["v"] = "discard"
        root.destroy()

    stop_btn = ttk.Button(buttons, text="Stop and apply best result", command=stop)
    stop_btn.grid(row=0, column=0, padx=(0, 8))
    ttk.Button(buttons, text="Cancel and discard", command=discard).grid(row=0, column=1)
    root.protocol("WM_DELETE_WINDOW", discard)

    def tick():
        finished = job.finished()
        if finished:
            job.wait()  # the reader thread has queued every event then
        for ev in job.events():
            state.update(ev)
        for v, (_, value) in zip(values, state.rows(time.monotonic() - t0)):
            v.set(value)
        bar["value"] = int(state.fraction * 1000)
        if finished:
            root.destroy()
            return
        root.after(POLL_MS, tick)

    root.after(POLL_MS, tick)
    root.mainloop()
    return choice["v"]
