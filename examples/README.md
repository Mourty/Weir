# Examples

Small programs that control Weir through its [control protocol](../docs/API.md),
and a client for each language to build your own on. Neither client needs
anything installed beyond Python 3.8 or Node.js 18: copy the one file into
your project.

Weir must be running. Everything here talks to it over the socket at
`$XDG_RUNTIME_DIR/weir/control.sock`, the same way the mixer window does.

## Python

| File | What it does |
|---|---|
| [`weir.py`](python/weir.py) | The client: `connect()`, `call()`, `batch()`, `subscribe()` and `events()`. |
| [`toggle_mute.py`](python/toggle_mute.py) | Mutes or unmutes a strip. Bind it to a key for a mute button that works everywhere. |
| [`mic_status.py`](python/mic_status.py) | Prints a line each time a strip is muted or unmuted, ready for a status bar such as Waybar. |
| [`level_meter.py`](python/level_meter.py) | A live level meter for a strip or bus, in the terminal. |

```sh
cd examples/python
./toggle_mute.py Mic
./level_meter.py Music
```

## JavaScript (Node.js)

| File | What it does |
|---|---|
| [`weir.js`](js/weir.js) | The client: `connect()`, `call()`, `batch()`, `subscribe()`, and events for every notification. |
| [`toggle_mute.js`](js/toggle_mute.js) | Mutes or unmutes a strip. |
| [`next_scene.js`](js/next_scene.js) | Loads the next saved scene: one button to step through them. |
| [`level_meter.js`](js/level_meter.js) | A live level meter for a strip, in the terminal. |

```sh
cd examples/js
./toggle_mute.js Mic
./next_scene.js
```

## Writing your own

Python:

```python
import weir

mixer = weir.connect()
mixer.call("set_strip", id="Music", gain_delta_db=-3)   # a little quieter
mixer.call("set_route", strip="Music", bus="B1")        # into the stream, or out
```

JavaScript:

```js
const weir = require("./weir");

const mixer = await weir.connect();
await mixer.call("set_strip", { id: "Music", gain_delta_db: -3 });
mixer.on("state_changed", (state) => console.log(state.strips.map((s) => s.name)));
await mixer.subscribe(["state"]);
```

Strips and buses can be named by their name (`"Music"`) or id (`2`), and
buses also by their label (`"A1"`, `"B1"`). Every method, with its
parameters and an example in each language, is in [docs/API.md](../docs/API.md).
