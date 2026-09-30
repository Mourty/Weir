#!/usr/bin/env python3
"""Print a line whenever a strip is muted or unmuted: a status bar module.

    ./mic_status.py Mic

Each line is JSON in the form Waybar's custom modules read, so it can go
straight into a bar; any program that reads lines will do. It prints the
current state first, then only changes, and uses no CPU in between.
"""

import json
import sys

import weir

if len(sys.argv) != 2:
    sys.exit(f"usage: {sys.argv[0]} STRIP")
name = sys.argv[1]


def show(strip):
    muted = strip["mute"]
    print(
        json.dumps(
            {
                "text": f"{strip['name']} {'off' if muted else 'on'}",
                "class": "muted" if muted else "live",
                "tooltip": f"{strip['name']} is {'muted' if muted else 'live'}",
            }
        ),
        flush=True,
    )


with weir.connect() as mixer:
    # Subscribe first, then read the state, so no change can slip in
    # between the two.
    mixer.subscribe("state")
    strip = weir.find(mixer.call("get_state")["mixer"]["strips"], name)
    if strip is None:
        sys.exit(f"weir: no strip called '{name}'")
    show(strip)
    for method, params in mixer.events():
        if method != "state_changed":
            continue
        now = weir.find(params["strips"], strip["id"])
        if now is not None and now["mute"] != strip["mute"]:
            show(now)
        strip = now or strip
