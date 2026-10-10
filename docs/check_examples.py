#!/usr/bin/env python3
"""Run every example in the documentation against a running Weir daemon.

    docs/check_examples.py --socket /tmp/weir-test/control.sock [DOC...]

Without documents it checks docs/API.md and docs/CLI.md.

Each shell, Python and JavaScript block is run on its own, after the mixer
is put back as it was when the check started, so the examples can rely on
the strips and buses a new install has. Use a daemon started with a fresh
configuration and with hotkeys' keys off, since the examples save scenes and
presets, change settings and add hotkeys:

    WEIR_HOTKEYS=none weir-daemon --config /tmp/weir-test/config.toml ...

Each example starts with one hotkey, "Mute mic", with no keys, one empty
group of hotkeys, "Streaming", and one sound of the user's own, "Airhorn".
Examples run in a folder holding airhorn.wav and applause.wav, and hotkeys'
sounds play on the first playback device, so there must be one: a null sink
will do.

The application examples need an application called Firefox to be playing,
and use 87 for its id; the check puts in its real one. For instance:

    cat /dev/zero | pw-cat --playback --target weir.input.3 \
        -P '{ application.name = "Firefox" }' -

A comment line before a block changes how it is checked:

    <!-- runs until stopped -->   it is meant to run forever; it passes if it
                                  is still running after two seconds
    <!-- not tested: why -->      it is only checked for syntax

JSON blocks are checked to be JSON. The daemon's answers are not compared
with anything: an example passes when it runs without an error.
"""

import argparse
import json
import math
import os
import re
import signal
import struct
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Leave no __pycache__ behind in examples/.
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "examples" / "python"))
import weir  # noqa: E402

BASELINE = "docs-check-baseline"
BASELINE_HOTKEY = {"name": "Mute mic", "steps": [
    {"method": "set_strip", "params": {"id": "Mic", "mute": "toggle"}}]}
BASELINE_SOUND = "Airhorn"
FOREVER_SECONDS = 2
TIMEOUT_SECONDS = 20


def blocks(markdown):
    """Yield (heading, language, marker, code, line) for each fenced block."""
    heading, marker = "", None
    lines = markdown.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("#"):
            heading = line.lstrip("#").strip()
        m = re.match(r"<!-- (.*?) -->$", line.strip())
        if m:
            marker = m.group(1)
        elif line.startswith("```"):
            language = line[3:].strip()
            start = i + 1
            i += 1
            while not lines[i].startswith("```"):
                i += 1
            yield heading, language, marker, "\n".join(lines[start:i]), start
            marker = None
        elif line.strip():
            marker = None
        i += 1


def write_sound(path, hz, seconds):
    """A short tone as a .wav, for the examples that add sounds."""
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(48000)
        frames = int(48000 * seconds)
        w.writeframes(b"".join(
            struct.pack("<h", int(8000 * math.sin(2 * math.pi * hz * i / 48000)))
            for i in range(frames)))


def reset(mixer, sounds_device, workdir):
    """Put the mixer, the settings, the rules, the hotkeys and the sounds back
    as they were."""
    mixer.batch(
        ("load_setup", {"name": BASELINE}),
        ("load_scene", {"name": BASELINE}),
        ("set_app_rules", {"rules": []}),
        ("set_settings", {"solo": "exclusive", "sample_rate": 0, "quantum": 0,
                          "meter_rate_hz": 30, "startup": "window",
                          "tray_icon": "color", "hotkey_popup": "popup",
                          "sounds_device": sounds_device, "sounds_volume_db": -12}),
    )
    clear_hotkeys(mixer)
    clear_sounds(mixer)
    mixer.call("add_sound", name=BASELINE_SOUND, path=str(workdir / "airhorn.wav"))
    mixer.call("set_hotkey", **BASELINE_HOTKEY)
    mixer.call("add_hotkey_group", name="Streaming")


def clear_sounds(mixer):
    """Remove every sound of the user's own."""
    for sound in mixer.call("list_hotkeys").get("sounds", []):
        if not sound.get("builtin"):
            mixer.call("remove_sound", name=sound["name"])


def clear_hotkeys(mixer):
    """Remove every hotkey and group of hotkeys."""
    info = mixer.call("list_hotkeys")
    for hotkey in info["hotkeys"]:
        mixer.call("remove_hotkey", hotkey=hotkey["id"])
    for group in info.get("groups", []):
        mixer.call("remove_hotkey_group", group=group["id"])


def run(argv, forever, env, cwd):
    """Run argv in `cwd`, where examples may write files; returns None on
    success, else what went wrong."""
    # A session of its own, so stopping it stops what it started too.
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, env=env, cwd=cwd, start_new_session=True)
    try:
        output, _ = proc.communicate(
            timeout=FOREVER_SECONDS if forever else TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.communicate()
        return None if forever else "timed out"
    if forever:
        return f"stopped when it should run on:\n{output}"
    if proc.returncode != 0:
        return f"exit {proc.returncode}:\n{output}"
    return None


def check(language, marker, code, app_id, env, workdir):
    """Check one block. Returns None when it passes, else the problem."""
    if language == "json":
        try:
            json.loads(code)
        except ValueError as e:
            return f"not JSON: {e}"
        return None
    untested = marker is not None and marker.startswith("not tested")
    forever = marker == "runs until stopped"
    if language == "sh":
        if untested:
            return None
        script = re.sub(r"(?<![\w.])87(?![\w.])", str(app_id), code)
        return run(["bash", "-e", "-c", script], forever, env, workdir)
    if language == "python":
        preamble = "" if "weir.connect(" in code else "mixer = weir.connect()\n"
        source = f"import weir\n{preamble}{code}\n"
        if untested:
            compile(source, "example", "exec")
            return None
        path = workdir / "example.py"
        path.write_text(source)
        return run([sys.executable, str(path)], forever, env, workdir)
    if language == "js":
        # The check brings its own copy of the client.
        code = code.replace('const weir = require("./weir");\n', "")
        preamble = "" if "weir.connect(" in code else "const mixer = await weir.connect();\n"
        closing = "" if forever else "mixer.close();\n"
        source = (
            f"const weir = require({json.dumps(str(ROOT / 'examples' / 'js' / 'weir.js'))});\n"
            f"(async () => {{\n{preamble}{code}\n{closing}}})()"
            ".catch((e) => { console.error(e); process.exit(1); });\n"
        )
        path = workdir / "example.js"
        path.write_text(source)
        if untested:
            return run(["node", "--check", str(path)], False, env, workdir)
        return run(["node", str(path)], forever, env, workdir)
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--socket", required=True, help="the test daemon's socket")
    parser.add_argument("docs", nargs="*", default=[str(ROOT / "docs" / d)
                                                    for d in ("API.md", "CLI.md")])
    args = parser.parse_args()

    # The examples connect wherever the daemon listens by default, so point
    # the default at the test daemon.
    runtime = Path(tempfile.mkdtemp(prefix="weir-docs-"))
    (runtime / "weir").mkdir()
    (runtime / "weir" / "control.sock").symlink_to(Path(args.socket).resolve())
    env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime),
               PATH=f"{ROOT / 'target' / 'release'}:{os.environ['PATH']}",
               PYTHONPATH=str(ROOT / "examples" / "python"),
               PYTHONDONTWRITEBYTECODE="1")

    mixer = weir.connect(args.socket)
    firefox = weir.find(mixer.call("list_apps"), "Firefox")
    if firefox is None:
        sys.exit("Play something as Firefox first; see the top of this file.")
    sink = next((d for d in mixer.call("list_devices") if d["kind"] == "sink"), None)
    if sink is None:
        sys.exit("Make a playback device for hotkeys' sounds first; see the top of this file.")
    write_sound(runtime / "airhorn.wav", 440, 0.5)
    write_sound(runtime / "applause.wav", 330, 0.8)
    mixer.call("save_setup", name=BASELINE)
    mixer.call("save_scene", name=BASELINE)

    failures, count = 0, 0
    for doc in args.docs:
        for heading, language, marker, code, line in blocks(Path(doc).read_text()):
            if language not in ("sh", "python", "js", "json"):
                continue
            count += 1
            reset(mixer, sink["name"], runtime)
            problem = check(language, marker, code, firefox["id"], env, runtime)
            if problem:
                failures += 1
                print(f"FAIL {doc}:{line} ({heading}, {language})\n{code}\n--> {problem}\n")

    reset(mixer, sink["name"], runtime)
    for kind in ("setup", "scene"):
        mixer.call(f"delete_{kind}", name=BASELINE)
    clear_hotkeys(mixer)
    clear_sounds(mixer)
    mixer.call("set_settings", sounds_device=None)
    print(f"{count - failures} of {count} examples passed")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
