#!/usr/bin/env python3
"""Mute or unmute a strip, and say which it is now.

    ./toggle_mute.py Mic

Bind it to a key in your desktop's shortcut settings for a push-to-mute
button that works in every application.
"""

import sys

import weir

if len(sys.argv) != 2:
    sys.exit(f"usage: {sys.argv[0]} STRIP")

with weir.connect() as mixer:
    try:
        strip = mixer.call("set_strip", id=sys.argv[1], mute="toggle")
    except weir.WeirError as e:
        sys.exit(f"weir: {e}")
    print(f"{strip['name']} is {'muted' if strip['mute'] else 'live'}")
