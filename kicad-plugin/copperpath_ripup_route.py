"""Entry point of the "Rip up and reroute selected nets" action (see plugin.json)."""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from copperpath_kicad import action  # noqa: E402

if __name__ == "__main__":
    sys.exit(action.run(command="ripup-route"))
