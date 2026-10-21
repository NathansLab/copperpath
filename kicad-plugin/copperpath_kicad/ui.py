"""Minimal dialogs. IPC plugins run as their own process, so a tkinter message box is used;
without tkinter the message goes to stderr (shown in KiCad's API log)."""

import sys


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
