#!/usr/bin/env python3
"""A live level meter for a strip or bus, in the terminal.

    ./level_meter.py Music
    ./level_meter.py A1

Shows the loudest channel, ten times a second, until you press Ctrl+C.
"""

import sys

import weir

FLOOR_DB = -60.0
WIDTH = 50

if len(sys.argv) != 2:
    sys.exit(f"usage: {sys.argv[0]} STRIP_OR_BUS")

with weir.connect() as mixer:
    state = mixer.call("get_state")["mixer"]
    key = sys.argv[1]
    strip = weir.find(state["strips"], key)
    buses = state["buses"]
    bus = weir.find(buses, key) or next(
        (b for b in buses if weir.bus_label(buses, b) == key.upper()), None
    )
    if strip is None and bus is None:
        sys.exit(f"weir: no strip or bus called '{key}'")
    kind, target = ("strips", strip) if strip else ("buses", bus)

    # Meters are sent 30 times a second by default; ten is plenty here, and
    # nothing is missed in between: each message holds the peaks since the
    # one before.
    mixer.subscribe("meters", meter_rate_hz=10)
    try:
        for method, params in mixer.events():
            if method != "meters":
                continue
            channels = params[kind].get(str(target["id"]), [])
            peak = max([FLOOR_DB, *channels])
            filled = round(WIDTH * (max(peak, FLOOR_DB) - FLOOR_DB) / -FLOOR_DB)
            bar = "#" * filled + "." * (WIDTH - filled)
            print(f"\r{target['name']:>12} [{bar}] {peak:6.1f} dB", end="", flush=True)
    except KeyboardInterrupt:
        print()
