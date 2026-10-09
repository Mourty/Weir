# Weir control protocol

Everything the mixer window can do, any program can do too. The window and
`weirctl` are ordinary clients of the daemon, using exactly the protocol
described here, so nothing is held back for them.

That makes Weir easy to build on:

* a **Stream Deck** or OpenDeck plugin with mute buttons that light up,
  faders on dials and a key per scene;
* **hotkeys** beyond the ones Weir has built in, or a button that shares
  one of them;
* a **status bar** module that shows whether you are live;
* a **MIDI controller** bridge, so real faders move the virtual ones;
* **scripts**: switch to your streaming scene when OBS starts, duck the
  music while you are in a call.

The protocol is JSON-RPC 2.0 over a Unix socket, one message per line. Any
language that can open a socket can use it, and ready-made clients for
Python and Node.js are in [`examples/`](../examples/).

## Contents

* [Quick start](#quick-start)
* [Talking to the daemon](#talking-to-the-daemon)
* [Conventions](#conventions)
* [Methods](#methods)
  * [The connection](#the-connection): `hello`, `ping`, `get_state`, `subscribe`, `unsubscribe`, `describe`
  * [Strips](#strips): `set_strip`, `add_strip`, `remove_strip`, `move_strip`
  * [Buses](#buses): `set_bus`, `add_bus`, `remove_bus`, `move_bus`
  * [Routing](#routing): `set_route`
  * [Devices and applications](#devices-and-applications): `list_devices`, `list_apps`, `move_app`, `set_app_volume`, `set_app_rules`
  * [Equalizer presets and the analyzer](#equalizer-presets-and-the-analyzer): `list_eq_presets`, `apply_eq_preset`, `save_eq_preset`, `delete_eq_preset`, `watch_spectrum`
  * [Scenes and setups](#scenes-and-setups): `save_scene`, `load_scene`, `list_scenes`, `delete_scene` and the same for setups
  * [Undo](#undo): `undo`, `redo`, `history`
  * [Hotkeys](#hotkeys): `list_hotkeys`, `set_hotkey`, `remove_hotkey`, `switch_hotkey`, `move_hotkey`, `run_hotkey`, `press_hotkey`, `release_hotkey`, `add_hotkey_group`, `set_hotkey_group`, `remove_hotkey_group`, `move_hotkey_group`, `open_shortcut_settings`
  * [Settings and the window](#settings-and-the-window): `set_settings`, `show_window`
* [Notifications](#notifications)
* [Types](#types)
* [Errors](#errors)
* [Recipes](#recipes)

## Quick start

Weir has to be running; starting the mixer window starts it.

From a terminal, `weirctl raw` sends any request and prints the answer:

```sh
weirctl raw set_strip '{"id": "Mic", "mute": "toggle"}'
```

That muted the strip called Mic, or unmuted it if it was muted. The answer
is the strip as it is now:

```json
{
  "gain_db": 0.0,
  "id": 1,
  "kind": "hardware",
  "layout": "mono",
  "mute": true,
  "name": "Mic",
  "pan": 0.0,
  "routes": [3],
  "solo": false
}
```

The same from Python, with [`examples/python/weir.py`](../examples/python/weir.py):

```python
import weir

mixer = weir.connect()
mixer.call("set_strip", id="Mic", mute="toggle")
```

And from Node.js, with [`examples/js/weir.js`](../examples/js/weir.js):

```js
const weir = require("./weir");

const mixer = await weir.connect();
await mixer.call("set_strip", { id: "Mic", mute: "toggle" });
```

The examples in this document use the strips and buses a new install starts
with:

| Strips | | Buses | |
|---|---|---|---|
| 1 | **Mic**: a microphone (hardware, mono) | 1 | **Headset** (A1): plays to a device |
| 2 | **Music**: a virtual input apps play into | 2 | **Speakers** (A2): plays to a device |
| 3 | **Browser**: another virtual input | 3 | **Stream Mic** (B1): a virtual microphone |
| 4 | **Soundboard**: another virtual input | | |

In the Python examples `mixer` is a connection from `weir.connect()`, and in
the JavaScript ones it is one from `await weir.connect()`.

## Talking to the daemon

### The socket

The daemon listens on a Unix socket at
`$XDG_RUNTIME_DIR/weir/control.sock`, which is usually
`/run/user/1000/weir/control.sock`. Without `XDG_RUNTIME_DIR` it is
`/tmp/weir-<uid>/control.sock`. Only your own user can open it, so there is
no password, and there is no network listener.

Any number of programs can be connected at once. Every one of them sees
every change, whoever made it.

### Messages

Each message is one line of JSON, UTF-8, ending in a newline, in both
directions. They follow [JSON-RPC 2.0](https://www.jsonrpc.org/specification).

A **request** names a method, carries its parameters, and has an `id` of
your choosing, a number or a string:

```json
{"jsonrpc": "2.0", "id": 1, "method": "set_strip", "params": {"id": "Music", "gain_db": -6}}
```

The **response** carries the same `id`, and either a `result`:

```json
{"jsonrpc": "2.0", "id": 1, "result": {"id": 2, "name": "Music", "gain_db": -6.0, "...": "..."}}
```

or an `error` (see [Errors](#errors)):

```json
{"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "no strip called 'Musci'"}}
```

A request without an `id` is carried out, but gets no response. That suits
a stream of changes where only the last one matters, such as a knob being
turned.

`params` can be left out of a method whose parameters are all optional, and
a method that takes none accepts `{}`, `[]` or nothing. The `jsonrpc`
member may be left out of requests; the daemon always sends it.

A **notification** is a message the daemon sends by itself, when something
changes. It has a `method` and `params` and no `id`:

```json
{"jsonrpc": "2.0", "method": "state_changed", "params": {"strips": ["..."], "buses": ["..."]}}
```

A connection only gets notifications for the topics it subscribed to (see
[`subscribe`](#subscribe)). They can arrive at any time, including between
a request and its response, so a client should read every line and sort
them by whether they have an `id`. Both example clients do that for you.

### Batches

A line can hold a **batch**: a JSON array of requests. They run one after
the other, in order, and the answer is one line with an array of their
responses (requests without an `id` leave no entry). One failing does not
stop the rest.

```json
[{"jsonrpc": "2.0", "id": 1, "method": "set_strip", "params": {"id": "Mic", "mute": false}},
 {"jsonrpc": "2.0", "id": 2, "method": "load_scene", "params": {"name": "Live"}}]
```

```python
mixer.batch(
    ("set_strip", {"id": "Mic", "mute": False}),
    ("set_route", {"strip": "Music", "bus": "B1", "enabled": False}),
)
```

```js
await mixer.batch([
  ["set_strip", { id: "Mic", mute: false }],
  ["set_route", { strip: "Music", bus: "B1", enabled: false }],
]);
```

### Versions

[`hello`](#hello) returns the protocol version, which only changes when
something changes that old clients would trip over, and a list of
`capabilities` such as `"ducking"` or `"spectrum"`. A client that wants to
work with older daemons as well can check for the features it uses there.

[`describe`](#describe) returns JSON Schemas of every request,
notification and the whole state, generated from the types the daemon
itself uses, for generating types in your language or checking requests
before sending them.

## Conventions

These hold for every method. They are what make most buttons and knobs a
single request that needs no reading of the state first.

### Strips and buses by name

Wherever a strip or bus goes, its **name** will do as well as its **id**,
ignoring case: `"Music"` or `2`. Buses also answer to their **label**, the
short name the mixer shows on the routing buttons: `A1`, `A2` and so on for
buses that play to a device, `B1`, `B2` for virtual ones, numbered in the
order the mixer shows them.

This works for the `id` of `set_strip`, `set_bus`, `remove_*` and
`move_*`, every `strip` and `bus` parameter, the keys of `sends`, ducking's
`triggers` and `buses`, a solo cue bus, the targets of `watch_spectrum`
and the steps of a hotkey.

Ids never change while a strip or bus exists, and are not reused while it
does. Names can be changed by the user, so a program that keeps a strip
for long should hold on to its id.

### Changing only what you name

Every method that changes something takes only the fields you want to
change, and leaves the rest as they are. That goes one level down too:
`{"id": "Mic", "gate": {"enabled": true}}` switches the gate on and keeps
its threshold and timing.

A few fields can be cleared, and take `null` for that: `"device": null`
takes a hardware strip's device away, and `"color": null` its color.

### Switches take `"toggle"`

Every on/off field in a change accepts `true`, `false` or `"toggle"`: a
strip's `mute`, `solo` and `subwoofer`, a bus's `mute` and `mono`, each
effect's `enabled`, a route's `enabled`, a downmix's `lfe`, and an
application's `mute`. `"toggle"` is what a button wants: it needs no idea
of the current state.

### Steps

Levels can be moved by an amount instead of set: `gain_delta_db` on
`set_strip` and `set_bus`, `delay_delta_ms` on `set_bus`, `pan_delta` on
`set_strip`, `level_delta_db` on `set_route` and `volume_delta_db` on
`set_app_volume`. That is what a dial wants. Given with the absolute
value, the step is applied after it.

### Units and ranges

Levels are in decibels (dB). A fader goes from -60 dB, which is silence, to
+12 dB, and 0 dB leaves the sound as it is. Every 6 dB halves or doubles
the level. Times are in milliseconds and frequencies in hertz.

Values outside a field's range are brought into it rather than refused: a
`gain_db` of 100 sets +12. A value of the wrong type is refused. Fields the
daemon does not know are ignored, so check the spelling when a change seems
to do nothing; [`describe`](#describe) can validate requests.

### Undo history

Changes to the mix (`set_strip`, `set_bus`, `set_route`, adding, removing
and moving strips and buses, loading a scene or setup, applying an
equalizer preset) can be undone, whoever made them. A run of changes to the
same thing less than 1.5 seconds apart is one step, so a dial turned for a
while undoes in one go. A press of a [hotkey](#hotkeys) is one step,
however many changes it makes. Application volumes, application rules,
hotkeys themselves and settings are not part of the history.

## Methods

Each method below lists its parameters and what it returns, with an example
in each of the three ways of calling it: `weirctl raw` from a shell, and the
Python and Node.js clients. Parameters marked *optional* can be left out.

### The connection

#### `hello`

Says which protocol version and features the daemon has. Takes no
parameters.

Returns `{"protocol_version", "daemon_version", "capabilities"}`:

```json
{
  "protocol_version": 1,
  "daemon_version": "1.0.0",
  "capabilities": ["meters", "apps", "eq", "gate", "denoise", "compressor", "ducking",
                   "limiter", "bus_delay", "sends", "upmix", "downmix", "spectrum", "history",
                   "scenes", "setups", "app_rules", "system_volumes",
                   "external_effects", "hotkeys", "toggle", "deltas", "names",
                   "batch", "describe"]
}
```

```sh
weirctl raw hello
```

```python
if "ducking" in mixer.call("hello")["capabilities"]:
    print("this daemon can duck")
```

```js
const { protocol_version } = await mixer.call("hello");
```

#### `ping`

Checks the daemon is there and answering. Returns `"pong"`.

```sh
weirctl raw ping
```

```python
assert mixer.call("ping") == "pong"
```

```js
await mixer.call("ping");
```

#### `get_state`

Everything the daemon knows, in one answer. Takes no parameters.

Returns a [FullState](#fullstate): the mixer's strips and buses, the
devices and applications, the engine's status, the settings, the equalizer
presets, the application rules, the saved scenes and setups, the
system's volume on Weir's devices, and whether external effects are
connected.

A client that shows the state reads it once with `get_state`, then keeps it
up to date from notifications. Subscribe first, then read the state, so no
change can slip in between.

```sh
weirctl raw get_state
```

```python
state = mixer.call("get_state")
for strip in state["mixer"]["strips"]:
    print(strip["name"], strip["gain_db"], "dB")
```

```js
const state = await mixer.call("get_state");
console.log(state.mixer.buses.map((b) => b.name));
```

#### `subscribe`

Start getting [notifications](#notifications) on this connection. They
stop when the connection closes.

| Parameter | Type | |
|---|---|---|
| `topics` | list of strings, *optional* | What to hear about, from the list below. Empty or left out: all of them but `window`. |
| `meter_rate_hz` | number, *optional* | Send meters to this connection at most this many times a second, fewer than the daemon makes (30 unless set otherwise). Nothing is lost in between: each message holds the peaks since the one before. |

| Topic | Notifications |
|---|---|
| `state` | `state_changed`, `eq_presets_changed`, `history_changed`, `app_rules_changed`, `library_changed` |
| `meters` | `meters`, up to 30 times a second |
| `devices` | `devices_changed`, `system_volumes_changed`, `inserts_changed` |
| `apps` | `apps_changed` |
| `engine` | `engine_changed` |
| `settings` | `settings_changed` |
| `hotkeys` | `hotkeys_changed` |
| `window` | `show_window`, `quit`. Only for a mixer window: subscribing to it tells the daemon this connection *is* one. There is one window at a time: while one is open, a second connection subscribing to `window` gets `quit` at once, and the open window gets `show_window`. |

Returns the list of topics now active on this connection.

<!-- runs until stopped -->
```sh
weirctl watch             # state, devices, apps and the engine, as they change
weirctl watch --meters    # meters too
```

<!-- runs until stopped -->
```python
mixer.subscribe("state", "meters", meter_rate_hz=10)
for method, params in mixer.events():
    print(method)
```

<!-- runs until stopped -->
```js
mixer.on("state_changed", (state) => console.log("the mix changed"));
mixer.on("meters", (meters) => console.log(meters.strips));
await mixer.subscribe(["state", "meters"], 10);
```

#### `unsubscribe`

Stop getting notifications for some topics.

| Parameter | Type | |
|---|---|---|
| `topics` | list of strings, *optional* | Topics to stop. Empty or left out: all of them. |

Returns the list of topics still active.

```sh
weirctl raw unsubscribe '{"topics": ["meters"]}'
```

```python
mixer.call("unsubscribe", topics=["meters"])
```

```js
await mixer.call("unsubscribe", { topics: ["meters"] });
```

#### `describe`

JSON Schemas (draft 2020-12) of the whole protocol, generated from the
types the daemon itself uses. Takes no parameters.

Returns `{"protocol_version", "requests", "notifications", "state"}`:
`requests` has one schema per method, with its documentation, `notifications`
one per notification, and `state` describes a [FullState](#fullstate).
Feed them to a code generator for typed clients, or to a validator to check
requests before sending them.

```sh
weirctl raw describe > weir-schema.json
```

```python
schemas = mixer.call("describe")
methods = [s["properties"]["method"]["const"] for s in schemas["requests"]["oneOf"]]
```

```js
const { requests } = await mixer.call("describe");
```

### Strips

A strip is an input: a microphone or other device (`hardware`), or a
virtual playback device that applications play into (`virtual`). Its sound
goes through its effects and its fader, then to the buses it is routed to.
See [Strip](#strip) for every field.

#### `set_strip`

Change a strip: its fader, pan, mute, name, effects, and so on. Only the
fields you give change.

| Parameter | Type | |
|---|---|---|
| `id` | id or name | The strip. |
| `gain_db` | number, *optional* | The fader, from -60 (silent) to +12 dB. |
| `gain_delta_db` | number, *optional* | Move the fader by this many dB. |
| `pan` | number, *optional* | Left to right, -1 to 1; 0 is the middle. Turns the other side down. |
| `pan_delta` | number, *optional* | Move the pan by this much. |
| `mute` | switch, *optional* | Silence the strip everywhere. |
| `solo` | switch, *optional* | Hear only the soloed strips. What that means exactly is up to the `solo` [setting](#settings). |
| `name` | string, *optional* | A new name, unique among strips, up to 40 characters. |
| `color` | string or `null`, *optional* | An accent color as `"#RRGGBB"`, or `null` for none. |
| `layout` | [layout](#layout), *optional* | Its channels, such as `"stereo"` or `"surround_5_1"`. |
| `device` | string or `null`, *optional* | For a hardware strip, the device to capture from, by the `name` [`list_devices`](#list_devices) gives; `null` for none. |
| `sends` | object, *optional* | The strip's level in individual buses' mixes, on top of its fader, as `{"B1": -6}`. Buses not listed keep theirs. |
| `upmix` | string, *optional* | How it plays on buses with more speakers than it has channels: `off`, `center`, `all_channel_stereo` or `passive_surround`. See [Upmix](#upmix). |
| `subwoofer` | switch, *optional* | Also play its bass on the subwoofer of buses that have one. |
| `denoise` | object, *optional* | Change its [noise suppression](#denoise). |
| `gate` | object, *optional* | Change its [noise gate](#gate). |
| `eq` | object, *optional* | Change its [equalizer](#equalizer). |
| `compressor` | object, *optional* | Change its [compressor](#compressor). |
| `ducking` | object, *optional* | Change its [ducking](#ducking). |
| `insert` | object, *optional* | Change its [external effects](#insert). |

Returns the [Strip](#strip) as it is now.

Changing a strip's name or layout recreates its virtual device, which takes
a moment; applications playing into it are moved back once it is there.
The same goes for its external effects' devices, and the links other
programs had with them are made again.

```sh
weirctl raw set_strip '{"id": "Music", "gain_db": -6}'           # fader to -6 dB
weirctl raw set_strip '{"id": "Music", "gain_delta_db": 1}'      # 1 dB up, for a dial
weirctl raw set_strip '{"id": "Music", "pan": -0.5}'             # halfway to the left
weirctl raw set_strip '{"id": "Music", "pan_delta": 0.1}'        # a little to the right
weirctl raw set_strip '{"id": "Mic", "mute": "toggle"}'          # a mute button
weirctl raw set_strip '{"id": "Mic", "solo": true}'
weirctl raw set_strip '{"id": "Browser", "name": "Chat"}'
weirctl raw set_strip '{"id": "Music", "color": "#3CBEAF"}'
weirctl raw set_strip '{"id": "Music", "color": null}'
weirctl raw set_strip '{"id": "Music", "sends": {"B1": -12}}'    # quieter on the stream only
weirctl raw set_strip '{"id": "Music", "layout": "surround_5_1"}'
weirctl raw set_strip '{"id": "Music", "upmix": "all_channel_stereo", "subwoofer": true}'
weirctl raw set_strip '{"id": "Mic", "device": "alsa_input.usb-Blue_Yeti-00.analog-stereo"}'
weirctl raw set_strip '{"id": "Mic", "device": null}'
```

Effects change the same way, each with only the fields you give:

```sh
weirctl raw set_strip '{"id": "Mic", "denoise": {"enabled": true}}'
weirctl raw set_strip '{"id": "Mic", "gate": {"enabled": true, "threshold_db": -40}}'
weirctl raw set_strip '{"id": "Mic", "compressor": {"enabled": true, "threshold_db": -20, "ratio": 3}}'
weirctl raw set_strip '{"id": "Mic", "eq": {"enabled": true, "bands": [{"kind": "high_pass", "freq_hz": 80, "q": 0.707}]}}'
weirctl raw set_strip '{"id": "Music", "ducking": {"enabled": true, "triggers": ["Mic"], "amount_db": 15}}'
weirctl raw set_strip '{"id": "Mic", "gate": {"enabled": "toggle"}}'
weirctl raw set_strip '{"id": "Mic", "insert": {"enabled": true, "position": "before_compressor"}}'
```

```python
mixer.call("set_strip", id="Music", gain_db=-6)
mixer.call("set_strip", id="Mic", gate={"enabled": True, "threshold_db": -40})

strip = mixer.call("set_strip", id="Mic", mute="toggle")
print(strip["name"], "is", "muted" if strip["mute"] else "live")
```

```js
await mixer.call("set_strip", { id: "Music", gain_delta_db: -1 });
await mixer.call("set_strip", { id: "Music", ducking: { enabled: true, triggers: ["Mic"] } });

const strip = await mixer.call("set_strip", { id: "Mic", mute: "toggle" });
console.log(`${strip.name} is ${strip.mute ? "muted" : "live"}`);
```

#### `add_strip`

Add a strip at the end.

| Parameter | Type | |
|---|---|---|
| `name` | string | Unique among strips, up to 40 characters. For a virtual strip it is also the device name applications see: "*name* (Weir)". |
| `kind` | string | `virtual`, a device applications can play into, or `hardware`, to capture from a device. |
| `layout` | [layout](#layout), *optional* | Its channels. Stereo when left out. |
| `device` | string, *optional* | For a hardware strip, the device to capture from, by the `name` [`list_devices`](#list_devices) gives. It can be set later. |
| `routes` | list of buses, *optional* | The buses to send it to. None when left out. |

Returns the new [Strip](#strip), with its id.

```sh
weirctl raw add_strip '{"name": "Game", "kind": "virtual", "routes": ["A1", "B1"]}'
weirctl raw add_strip '{"name": "Guitar", "kind": "hardware", "layout": "mono"}'
```

```python
game = mixer.call("add_strip", name="Game", kind="virtual", routes=["A1"])
print("added strip", game["id"])
```

```js
const game = await mixer.call("add_strip", { name: "Game", kind: "virtual", routes: ["A1"] });
```

#### `remove_strip`

Remove a strip. Applications playing into it go back to the system's
default output, and it disappears from other strips' ducking. Like any
change to the mix, it can be undone.

| Parameter | Type | |
|---|---|---|
| `id` | id or name | The strip. |

Returns `null`.

```sh
weirctl raw add_strip '{"name": "Game", "kind": "virtual"}'
weirctl raw remove_strip '{"id": "Game"}'
```

```python
mixer.call("add_strip", name="Game", kind="virtual")
mixer.call("remove_strip", id="Game")
```

```js
await mixer.call("add_strip", { name: "Game", kind: "virtual" });
await mixer.call("remove_strip", { id: "Game" });
```

#### `move_strip`

Move a strip to another place in the row. The order is the order the mixer
shows strips in, left to right.

| Parameter | Type | |
|---|---|---|
| `id` | id or name | The strip. |
| `index` | number | Where it goes, counting from 0. Past the end means last. |

Returns the ids of every strip, in their new order.

```sh
weirctl raw move_strip '{"id": "Mic", "index": 99}'    # to the end: [2, 3, 4, 1]
```

```python
mixer.call("move_strip", id="Soundboard", index=0)
```

```js
await mixer.call("move_strip", { id: "Soundboard", index: 0 });
```

### Buses

A bus is an output: a device to play to, such as headphones or speakers
(`hardware`), or a virtual microphone that other applications can record
from (`virtual`), such as the one a streaming program or a call uses. It
plays the mix of every strip routed to it, through its equalizer, its
fader and its safety limiter. See [Bus](#bus) for every field.

#### `set_bus`

Change a bus. Only the fields you give change.

| Parameter | Type | |
|---|---|---|
| `id` | id, name or label | The bus: `3`, `"Stream Mic"` or `"B1"`. |
| `gain_db` | number, *optional* | The fader, from -60 (silent) to +12 dB. |
| `gain_delta_db` | number, *optional* | Move the fader by this many dB. |
| `mute` | switch, *optional* | Silence the bus. |
| `mono` | switch, *optional* | Fold every channel into one, on all its speakers. |
| `delay_ms` | number, *optional* | Hold the bus's output back by this many milliseconds, 0 to 500. Values outside that are brought into it. |
| `delay_delta_ms` | number, *optional* | Move the delay by this many milliseconds. |
| `name` | string, *optional* | A new name, unique among buses, up to 40 characters. |
| `color` | string or `null`, *optional* | An accent color as `"#RRGGBB"`, or `null` for none. |
| `layout` | [layout](#layout), *optional* | Its channels. |
| `device` | string or `null`, *optional* | For a hardware bus, the device to play to, by the `name` [`list_devices`](#list_devices) gives; `null` for none. |
| `eq` | object, *optional* | Change its [equalizer](#equalizer). |
| `limiter` | object, *optional* | Change its [safety limiter](#limiter). |
| `downmix` | object, *optional* | Change how it plays channels it has no speaker for. See [Downmix](#downmix). |
| `insert` | object, *optional* | Change its [external effects](#insert). |

Returns the [Bus](#bus) as it is now.

```sh
weirctl raw set_bus '{"id": "A1", "gain_db": -3}'
weirctl raw set_bus '{"id": "A1", "gain_delta_db": -1}'
weirctl raw set_bus '{"id": "Speakers", "mute": "toggle"}'
weirctl raw set_bus '{"id": "B1", "mono": true}'
weirctl raw set_bus '{"id": "A1", "delay_ms": 180}'
weirctl raw set_bus '{"id": "A1", "delay_delta_ms": -5}'
weirctl raw set_bus '{"id": "B1", "limiter": {"ceiling_db": -3}}'
weirctl raw set_bus '{"id": "A2", "layout": "surround_5_1", "downmix": {"method": "matrix"}}'
weirctl raw set_bus '{"id": "A1", "eq": {"enabled": true, "bands": [{"kind": "low_shelf", "freq_hz": 100, "gain_db": 4, "q": 0.707}]}}'
weirctl raw set_bus '{"id": "A1", "device": "alsa_output.usb-SteelSeries_Arctis-00.analog-stereo"}'
weirctl raw set_bus '{"id": "B1", "insert": {"enabled": true, "position": "before_limiter", "fallback": "silence"}}'
```

```python
mixer.call("set_bus", id="A1", gain_delta_db=-1)
mixer.call("set_bus", id="Stream Mic", limiter={"enabled": True, "ceiling_db": -1})
```

```js
await mixer.call("set_bus", { id: "Speakers", mute: "toggle" });
```

#### `add_bus`

Add a bus at the end.

| Parameter | Type | |
|---|---|---|
| `name` | string | Unique among buses, up to 40 characters. For a virtual bus it is also the microphone name applications see: "*name* (Weir)". |
| `kind` | string | `hardware`, playing to a device, or `virtual`, a virtual microphone. |
| `layout` | [layout](#layout), *optional* | Its channels. Stereo when left out. |
| `device` | string, *optional* | For a hardware bus, the device to play to. It can be set later. |

Returns the new [Bus](#bus), with its id. New virtual buses start with
their limiter on: whatever records them would clip anything over 0 dB.

```sh
weirctl raw add_bus '{"name": "Recording", "kind": "virtual"}'
weirctl raw add_bus '{"name": "Living room", "kind": "hardware", "layout": "surround_5_1"}'
```

```python
recording = mixer.call("add_bus", name="Recording", kind="virtual")
```

```js
const recording = await mixer.call("add_bus", { name: "Recording", kind: "virtual" });
```

#### `remove_bus`

Remove a bus. Strips routed to it lose that route, and ducking forgets it.
If solo was set to cue on it, solo silences the other strips everywhere
until another cue bus is chosen.

| Parameter | Type | |
|---|---|---|
| `id` | id, name or label | The bus. |

Returns `null`.

```sh
weirctl raw add_bus '{"name": "Recording", "kind": "virtual"}'
weirctl raw remove_bus '{"id": "Recording"}'
```

```python
mixer.call("add_bus", name="Recording", kind="virtual")
mixer.call("remove_bus", id="Recording")
```

```js
await mixer.call("add_bus", { name: "Recording", kind: "virtual" });
await mixer.call("remove_bus", { id: "Recording" });
```

#### `move_bus`

Move a bus to another place in the row. This also changes the labels:
buses are numbered in the order they are shown.

| Parameter | Type | |
|---|---|---|
| `id` | id, name or label | The bus. |
| `index` | number | Where it goes, counting from 0. Past the end means last. |

Returns the ids of every bus, in their new order.

```sh
weirctl raw move_bus '{"id": "Speakers", "index": 0}'   # Speakers becomes A1
```

```python
mixer.call("move_bus", id="Speakers", index=0)
```

```js
await mixer.call("move_bus", { id: "Speakers", index: 0 });
```

### Routing

#### `set_route`

Send a strip to a bus, stop sending it, or set how loud it is in that
bus's mix. A strip's level in a mix is on top of its fader, so a strip can
be quieter on the stream than in your headphones.

| Parameter | Type | |
|---|---|---|
| `strip` | id or name | The strip. |
| `bus` | id, name or label | The bus. |
| `enabled` | switch, *optional* | Send it (`true`), stop (`false`), or `"toggle"`. Left out, the route toggles, unless one of the levels below is given. |
| `level_db` | number, *optional* | Its level in this bus's mix, -60 to +12 dB. Leaves the route on or off as it is. |
| `level_delta_db` | number, *optional* | Move that level by this many dB. |

Returns the [Strip](#strip) as it is now; its `routes` lists the buses it
is sent to and `sends` the levels that are not 0 dB. A level is kept while
its route is off.

```sh
weirctl raw set_route '{"strip": "Music", "bus": "B1"}'                        # toggle
weirctl raw set_route '{"strip": "Mic", "bus": "A1", "enabled": true}'
weirctl raw set_route '{"strip": "Music", "bus": "B1", "level_db": -12}'
weirctl raw set_route '{"strip": "Music", "bus": "B1", "level_delta_db": 3}'
```

```python
music = mixer.call("set_route", strip="Music", bus="B1")
print("Music on stream:", 3 in music["routes"])
```

```js
await mixer.call("set_route", { strip: "Music", bus: "B1", level_delta_db: -3 });
```

### Devices and applications

#### `list_devices`

The devices hardware strips can capture from and hardware buses can play
to. Takes no parameters.

Returns a list of [DeviceInfo](#deviceinfo). Use a device's `name` to set a
strip's or bus's `device`: it stays the same across reboots, unlike its
`id`.

```sh
weirctl raw list_devices
```

```python
for device in mixer.call("list_devices"):
    print(device["kind"], device["name"], device["description"])
```

```js
const mics = (await mixer.call("list_devices")).filter((d) => d.kind === "source");
```

#### `list_apps`

The applications playing sound right now, into Weir's strips or anywhere
else. Takes no parameters.

Returns a list of [AppStream](#appstream). Each has an `id`, which is only
good while it plays: PipeWire gives the id of a stream that ended to the
next one. Find applications by `name` (or `binary`) and read the id fresh.

```sh
weirctl raw list_apps
```

```python
for app in mixer.call("list_apps"):
    print(app["id"], app["name"], "on strip", app.get("strip"))
```

```js
const firefox = weir.find(await mixer.call("list_apps"), "Firefox");
```

#### `move_app`

Move an application to a virtual strip, as if it had picked that device
itself.

| Parameter | Type | |
|---|---|---|
| `app` | number | The application's `id`, from [`list_apps`](#list_apps). |
| `strip` | id or name | A virtual strip. |

Returns `null`. The move shows in the next `apps_changed`.

```sh
weirctl raw move_app '{"app": 87, "strip": "Browser"}'
```

```python
firefox = weir.find(mixer.call("list_apps"), "Firefox")
mixer.call("move_app", app=firefox["id"], strip="Browser")
```

```js
const firefox = weir.find(await mixer.call("list_apps"), "Firefox");
await mixer.call("move_app", { app: firefox.id, strip: "Browser" });
```

#### `set_app_volume`

Change an application's own volume or mute: the one the system's volume
control shows for it. It is separate from the strip's fader; the two
multiply.

| Parameter | Type | |
|---|---|---|
| `app` | number | The application's `id`, from [`list_apps`](#list_apps). |
| `volume_db` | number, *optional* | Its volume, -60 to +12 dB; 0 is full volume. |
| `volume_delta_db` | number, *optional* | Move its volume by this many dB. |
| `mute` | switch, *optional* | Mute or unmute it. |

At least one of the last three is needed. Returns `null`; the new volume
shows in the next `apps_changed`.

```sh
weirctl raw set_app_volume '{"app": 87, "volume_db": -6}'
weirctl raw set_app_volume '{"app": 87, "mute": "toggle"}'
```

```python
firefox = weir.find(mixer.call("list_apps"), "Firefox")
mixer.call("set_app_volume", app=firefox["id"], volume_delta_db=-3)
```

```js
const firefox = weir.find(await mixer.call("list_apps"), "Firefox");
await mixer.call("set_app_volume", { app: firefox.id, mute: "toggle" });
```

#### `set_app_rules`

Say where applications go when they start playing. A rule matches an
application by its `name` or `binary`, ignoring case, and moves it to a
strip as soon as it appears. After that it stays wherever it is put, so
moving it by hand is respected.

| Parameter | Type | |
|---|---|---|
| `rules` | list of [AppRule](#apprule) | Every rule: this replaces the ones there are. The first rule that matches wins. |

Returns the rules as stored. To add one rule, read the current ones from
`get_state` (`app_rules`), add to them and send them all back.

```sh
weirctl raw set_app_rules '{"rules": [{"app": "Spotify", "strip": "Music"}, {"app": "Discord", "strip": "Browser"}]}'
weirctl raw set_app_rules '{"rules": []}'     # no rules
```

```python
rules = mixer.call("get_state")["app_rules"]
rules.append({"app": "Spotify", "strip": "Music"})
mixer.call("set_app_rules", rules=rules)
```

```js
const { app_rules } = await mixer.call("get_state");
await mixer.call("set_app_rules", { rules: [...app_rules, { app: "Spotify", strip: "Music" }] });
```

### Equalizer presets and the analyzer

Weir comes with equalizer presets for common jobs, such as clearer speech
or taking the rumble out of a microphone, and users can save their own.
Applying one replaces a strip's or bus's bands and switches its equalizer
on.

#### `list_eq_presets`

Takes no parameters. Returns a list of [EqPreset](#eqpreset), the built-in
ones first.

```sh
weirctl raw list_eq_presets
```

```python
names = [p["name"] for p in mixer.call("list_eq_presets")]
```

```js
const mine = (await mixer.call("list_eq_presets")).filter((p) => !p.builtin);
```

#### `apply_eq_preset`

Replace a strip's or bus's equalizer bands with a preset's, and switch its
equalizer on.

| Parameter | Type | |
|---|---|---|
| `name` | string | The preset, ignoring case. |
| `strip` | id or name, *optional* | The strip to apply it to. |
| `bus` | id, name or label, *optional* | Or the bus. Give exactly one of the two. |

Returns the [Strip](#strip) or [Bus](#bus) as it is now.

```sh
weirctl raw apply_eq_preset '{"name": "Voice: clarity", "strip": "Mic"}'
weirctl raw apply_eq_preset '{"name": "Voice: remove rumble", "bus": "B1"}'
```

```python
mixer.call("apply_eq_preset", name="Voice: clarity", strip="Mic")
```

```js
await mixer.call("apply_eq_preset", { name: "Voice: clarity", strip: "Mic" });
```

#### `save_eq_preset`

Save bands as a preset of your own, replacing one of yours with the same
name. Presets are kept in `~/.config/weir/eq-presets.toml`.

| Parameter | Type | |
|---|---|---|
| `name` | string | 1 to 40 characters. The names of built-in presets are taken. |
| `bands` | list of [EqBand](#eqband) | Up to 16. |

Returns every preset, as `list_eq_presets` does.

```sh
weirctl raw save_eq_preset '{"name": "My mic", "bands": [{"kind": "high_pass", "freq_hz": 90, "q": 0.707}, {"kind": "peak", "freq_hz": 3000, "gain_db": 2, "q": 1}]}'
```

```python
# Keep what the microphone's equalizer is set to now.
mic = weir.find(mixer.call("get_state")["mixer"]["strips"], "Mic")
mixer.call("save_eq_preset", name="My mic", bands=mic.get("eq", {}).get("bands", []))
```

```js
await mixer.call("save_eq_preset", {
  name: "Warm",
  bands: [{ kind: "low_shelf", freq_hz: 200, gain_db: 3, q: 0.707 }],
});
```

#### `delete_eq_preset`

Delete a preset of your own. Built-in ones cannot be deleted.

| Parameter | Type | |
|---|---|---|
| `name` | string | The preset, ignoring case. |

Returns every preset that is left.

```sh
weirctl raw save_eq_preset '{"name": "Warm", "bands": []}'
weirctl raw delete_eq_preset '{"name": "Warm"}'
```

```python
mixer.call("save_eq_preset", name="Warm", bands=[])
mixer.call("delete_eq_preset", name="Warm")
```

```js
await mixer.call("save_eq_preset", { name: "Warm", bands: [] });
await mixer.call("delete_eq_preset", { name: "Warm" });
```

#### `watch_spectrum`

Ask for a live spectrum of what goes into and comes out of the equalizer
of some strips and buses, for drawing an analyzer. They arrive as
`spectrum` [notifications](#notifications), 30 a second for each, on this
connection only, whatever it subscribed to. They cost the daemon some work,
so it only makes the ones someone is watching.

| Parameter | Type | |
|---|---|---|
| `targets` | list of [StripOrBus](#striporbus) | Up to 32, as `{"strip": "Mic"}` or `{"bus": "A1"}`. This replaces the list; an empty one stops them all. |

Returns the targets now watched.

```sh
weirctl raw watch_spectrum '{"targets": [{"strip": "Mic"}]}'
```

<!-- runs until stopped -->
```python
mixer.call("watch_spectrum", targets=[{"strip": "Mic"}])
for method, params in mixer.events():
    if method == "spectrum":
        loudest = max(params["output_db"])
        print(f"loudest band: {loudest:.0f} dB")
```

<!-- runs until stopped -->
```js
mixer.on("spectrum", (s) => console.log(Math.max(...s.output_db)));
await mixer.call("watch_spectrum", { targets: [{ bus: "A1" }] });
```

### Scenes and setups

Both save how things are, under a name, to come back to later.

* A **scene** is the mix: every strip's and bus's level, mute, solo, pan,
  routes, send levels and effects. Loading one changes the sound and
  nothing else, so scenes are good for moments: "Gaming", "Streaming",
  "Late night". A scene is applied to strips and buses by name, so it keeps
  working after they are moved around or a device changes.
* A **setup** is the whole mixer: which strips and buses there are, their
  devices, names, layouts and colors. Loading one keeps the mix of every
  strip and bus that is in both, so a setup is good for places: "Desk",
  "Living room", "Travel".

Names are up to 64 letters, digits, spaces, `_`, `-` and `.`, and cannot
start with a dot. Saving under a name that is taken replaces it. Scenes are
kept in `~/.config/weir/scenes/`, setups in `~/.config/weir/setups/`, one
TOML file each.

#### `save_scene` and `save_setup`

Save the current mix as a scene, or the whole mixer as a setup.

| Parameter | Type | |
|---|---|---|
| `name` | string | Its name. |

Returns the [Library](#library): every scene and setup, and which is
current.

```sh
weirctl raw save_scene '{"name": "Streaming"}'
weirctl raw save_setup '{"name": "Desk"}'
```

```python
mixer.call("save_scene", name="Streaming")
```

```js
await mixer.call("save_setup", { name: "Desk" });
```

#### `load_scene` and `load_setup`

Bring back a saved scene or setup. Loading can be undone.

| Parameter | Type | |
|---|---|---|
| `name` | string | Its name. |

Returns the whole mixer as it is now, a [MixerState](#mixerstate).

```sh
weirctl raw save_scene '{"name": "Streaming"}'
weirctl raw load_scene '{"name": "Streaming"}'
```

```python
mixer.call("save_setup", name="Desk")
mixer.call("load_setup", name="Desk")
```

```js
await mixer.call("save_scene", { name: "Streaming" });
await mixer.call("load_scene", { name: "Streaming" });
```

#### `list_scenes` and `list_setups`

Take no parameters. Return the saved names, in alphabetical order. The
current ones are in the [Library](#library), in `get_state`.

```sh
weirctl raw list_scenes
```

```python
print(mixer.call("list_setups"))
```

```js
const scenes = await mixer.call("list_scenes");
```

#### `delete_scene` and `delete_setup`

| Parameter | Type | |
|---|---|---|
| `name` | string | Its name. |

Returns the [Library](#library) as it is now.

```sh
weirctl raw save_scene '{"name": "Old"}'
weirctl raw delete_scene '{"name": "Old"}'
```

```python
mixer.call("save_setup", name="Old")
mixer.call("delete_setup", name="Old")
```

```js
await mixer.call("save_scene", { name: "Old" });
await mixer.call("delete_scene", { name: "Old" });
```

### Undo

Changes to the mix are kept, up to 100 of them, whoever made them, and can
be undone and redone; see [Undo history](#undo-history) under Conventions
for what counts.

#### `undo` and `redo`

| Parameter | Type | |
|---|---|---|
| `steps` | number, *optional* | How many steps, 1 to 100. One when left out. |

Returns the [HistoryInfo](#historyinfo) as it is now. Asking for more steps
than there are takes as many as there are; with none at all it is an error.

```sh
weirctl raw set_strip '{"id": "Music", "gain_db": -20}'
weirctl raw undo
weirctl raw redo
```

```python
mixer.call("set_strip", id="Music", mute=True)
mixer.call("set_strip", id="Music", gain_db=-20)
mixer.call("undo", steps=2)
```

```js
await mixer.call("set_strip", { id: "Music", mute: true });
await mixer.call("undo");
```

#### `history`

What can be undone and redone. Takes no parameters.

Returns a [HistoryInfo](#historyinfo): each step's label, such as
`"Music fader"`, and when it happened.

```sh
weirctl raw history
```

```python
history = mixer.call("history")
if history["undo"]:
    print("Undo would take back:", history["undo"][0]["label"])
```

```js
const { undo } = await mixer.call("history");
```

### Hotkeys

A hotkey is keys and what they do: a list of steps, each a request as this
document describes them, such as `set_strip` with `"mute": "toggle"`. The
daemon watches for the keys wherever the focus is (how depends on the
desktop; see [KeysStatus](#keysstatus)). Hotkeys can also be pressed by
name, with keys or without, so a key on the keyboard and a Stream Deck
button can share one.

What a press does:

* every step, in order; or with `"each_press": "next"`, only the next one,
  going round, to step through presets or scenes with one key;
* with `repeat_ms`, the steps again and again while the keys are held, to
  turn the music down by holding a key;
* when the keys are let go, as `on_release` says: nothing, put back what
  the press changed (`restore`, for push to talk), or steps of its own;
* a step with `over_ms` fades: `gain_db` in `set_strip` and `set_bus`, and
  `level_db` in `set_route`, move there gradually.

Each press is one step in the [undo history](#undo-history), labelled
"NAME (hotkey)", from the keys going down until they are up and its fades
have finished, however many changes it made. One that puts everything back
leaves nothing to undo. Putting back sets back only what the hotkey itself
changed: a fader moved by hand while the key was held stays where it was
put, and undoing that fader afterwards does not bring the hotkey's change
back either.

Steps can name strips and buses; the daemon keeps their ids, so renaming
one does not break a hotkey. A hotkey working on one that was removed is
listed in `problems`.

There is one list of hotkeys, whatever scene or setup is loaded, in an
order of its own: first the hotkeys in no group, then each group with its
hotkeys. A group is switched on and off as one: while it is off, none of
its hotkeys' keys work, and each keeps its own switch for when the group
is on again. The daemon keeps the list in `~/.config/weir/hotkeys.json`.

#### `list_hotkeys`

Every hotkey, how keys reach Weir on this desktop, and anything wrong.
Takes no parameters. Returns a [HotkeysInfo](#hotkeysinfo).

```sh
weirctl raw list_hotkeys
```

```python
info = mixer.call("list_hotkeys")
print(info["keys"]["message"])
for hotkey in info["hotkeys"]:
    print(hotkey["name"], ", ".join(hotkey.get("keys", [])) or "(no keys)")
```

```js
const { hotkeys, keys } = await mixer.call("list_hotkeys");
console.log(keys.method, hotkeys.length);
```

#### `set_hotkey`

Add a hotkey, or replace one. The parameters are the [Hotkey](#hotkey):

| Parameter | Type | |
|---|---|---|
| `id` | number, *optional* | The hotkey to replace, whole. Left out, or 0, adds a new one. |
| `name` | string | Unique, ignoring case, up to 60 characters. |
| `keys` | list of strings, *optional* | Such as `["Ctrl+Alt+M"]`, or several, `["F9", "Ctrl+Alt+T"]`, any of which presses the hotkey: up to 8. See [Keys](#keys). One may be given as a string, `"F9"`. Left out or empty, the hotkey is pressed only by name. Where the desktop looks after the keys, it gets all of them when [KeysStatus](#keysstatus) says `settable` (KDE Plasma), and otherwise only the first is suggested to it, more being added in its settings. |
| `group` | number, *optional* | The [group](#add_hotkey_group) it is in, by id, or `0` (the default) for none. Changed to another group, it goes last in it. |
| `enabled` | boolean, *optional* | `false` switches its keys off; it can still be pressed by name. `true` when left out. Where the desktop looks after the keys, it keeps them for a hotkey switched off, once it has had them; with `settable`, it lets other programs use them meanwhile. |
| `steps` | list of [HotkeyStep](#hotkeystep) | What a press does, up to 32. |
| `each_press` | string, *optional* | `all`: every step at each press (the default). `next`: the next step only, back to the first after the last. |
| `on_release` | string, *optional* | What letting go does: `nothing` (the default), `restore` (put back what the press changed) or `steps` (do `release_steps`). |
| `release_steps` | list of [HotkeyStep](#hotkeystep), *optional* | With `on_release` `steps`: what letting go does, up to 32. |
| `repeat_ms` | number, *optional* | Do the steps again every this many milliseconds while the keys are held, 20 to 2000. The first repeat waits 400 ms, like a keyboard's, or `repeat_ms` if that is longer. |

Returns the Hotkey as saved: with its id, its keys as a list written the
usual way, and strips and buses by id.

A step can be any request that changes something; requests that only ask
(`get_state`, `list_*`, `history`), `subscribe`, `watch_spectrum` and the
hotkey methods themselves cannot be steps. A step is checked as the request
would be, so a misspelled strip is an error now rather than a dead key
later. Two hotkeys cannot have the same name, or share any keys.

```sh
weirctl raw set_hotkey '{"name": "Push to talk", "keys": ["F9", "Ctrl+Alt+T"], "steps": [{"method": "set_strip", "params": {"id": "Mic", "mute": false}}], "on_release": "restore"}'
```

Two equalizer presets, a press each, with no keys: for a Stream Deck
button.

```python
mixer.call("set_hotkey", name="Mic sound", each_press="next", steps=[
    {"method": "apply_eq_preset", "params": {"name": "Voice: clarity", "strip": "Mic"}},
    {"method": "apply_eq_preset", "params": {"name": "Voice: remove rumble", "strip": "Mic"}},
])
```

The music dipped while a key is held, gently:

```js
await mixer.call("set_hotkey", {
  name: "Dip the music",
  keys: ["Ctrl+Alt+D"],
  steps: [{ method: "set_strip", params: { id: "Music", gain_db: -20 }, over_ms: 300 }],
  on_release: "restore",
});
```

To change one, send it back with its id:

```python
hotkey = weir.find(mixer.call("list_hotkeys")["hotkeys"], "Mute mic")
hotkey["keys"] = ["Ctrl+Alt+M"]
mixer.call("set_hotkey", **hotkey)
```

#### `remove_hotkey`

| Parameter | Type | |
|---|---|---|
| `hotkey` | number or string | The hotkey's id or name. |

Returns the [HotkeysInfo](#hotkeysinfo) as it is now.

```sh
weirctl raw remove_hotkey '{"hotkey": "Mute mic"}'
```

```python
mixer.call("remove_hotkey", hotkey="Mute mic")
```

```js
await mixer.call("remove_hotkey", { hotkey: "Mute mic" });
```

#### `switch_hotkey`

Switch a hotkey's keys on or off, leaving the rest of it as it is.

| Parameter | Type | |
|---|---|---|
| `hotkey` | number or string | The hotkey's id or name. |
| `enabled` | boolean or `"toggle"` | On, off, or the opposite of what it is. |

Returns the [Hotkey](#hotkey).

```sh
weirctl raw switch_hotkey '{"hotkey": "Mute mic", "enabled": false}'
```

```python
mixer.call("switch_hotkey", hotkey="Mute mic", enabled="toggle")
```

```js
await mixer.call("switch_hotkey", { hotkey: "Mute mic", enabled: true });
```

#### `move_hotkey`

Move a hotkey to another place in the list, or into a group.

| Parameter | Type | |
|---|---|---|
| `hotkey` | number or string | The hotkey's id or name. |
| `group` | number or string, *optional* | The group to move it into, by id or name, or `0` for none. Left out, it stays in its group. |
| `index` | number, *optional* | Its place among the hotkeys of its group, or of no group, counting from 0. Past the end, or left out, means last. |

Returns the [HotkeysInfo](#hotkeysinfo) as it is now.

```sh
weirctl raw move_hotkey '{"hotkey": "Mute mic", "group": "Streaming"}'
weirctl raw move_hotkey '{"hotkey": "Mute mic", "group": 0, "index": 0}'
```

```python
mixer.call("move_hotkey", hotkey="Mute mic", index=0)
```

```js
await mixer.call("move_hotkey", { hotkey: "Mute mic", group: "Streaming", index: 0 });
```

#### `run_hotkey`, `press_hotkey` and `release_hotkey`

Do what a hotkey's keys do: `run_hotkey` is a tap, down and up at once;
`press_hotkey` is the keys going down and `release_hotkey` them coming up,
for a button that is held. They work whether the hotkey has keys or not,
and whether they are switched on.

| Parameter | Type | |
|---|---|---|
| `hotkey` | number or string | The hotkey's id or name. |

Returns the [Hotkey](#hotkey) once its steps are done, so the next request
sees what they changed; fades carry on after. Pressing a hotkey already
held, or letting go of one that is not, does nothing.

```sh
weirctl raw run_hotkey '{"hotkey": "Mute mic"}'
```

```python
mixer.call("press_hotkey", hotkey="Mute mic")
mixer.call("release_hotkey", hotkey="Mute mic")
```

```js
await mixer.call("run_hotkey", { hotkey: "Mute mic" });
```

#### `add_hotkey_group`

Add a group of hotkeys, last in the list. Hotkeys go into it with
[`move_hotkey`](#move_hotkey), or with `group` in
[`set_hotkey`](#set_hotkey).

| Parameter | Type | |
|---|---|---|
| `name` | string | Unique among groups, ignoring case, up to 60 characters. |
| `enabled` | boolean, *optional* | Whether its hotkeys' keys work. `true` when left out. |

Returns the [HotkeyGroup](#hotkeygroup), with its id.

```sh
weirctl raw add_hotkey_group '{"name": "Games"}'
```

```python
games = mixer.call("add_hotkey_group", name="Games", enabled=False)
mixer.call("move_hotkey", hotkey="Mute mic", group=games["id"])
```

```js
const group = await mixer.call("add_hotkey_group", { name: "Games" });
```

#### `set_hotkey_group`

Rename a group of hotkeys, or switch it on or off. Only what is given
changes.

| Parameter | Type | |
|---|---|---|
| `group` | number or string | The group's id or name. |
| `name` | string, *optional* | A new name. |
| `enabled` | boolean or `"toggle"`, *optional* | Switch its hotkeys' keys on or off. Each hotkey keeps its own switch. |

Returns the [HotkeyGroup](#hotkeygroup).

```sh
weirctl raw set_hotkey_group '{"group": "Streaming", "enabled": "toggle"}'
```

```python
mixer.call("set_hotkey_group", group="Streaming", enabled=False)
```

```js
await mixer.call("set_hotkey_group", { group: "Streaming", name: "Live" });
```

#### `remove_hotkey_group`

Remove a group of hotkeys. Its hotkeys stay, in no group.

| Parameter | Type | |
|---|---|---|
| `group` | number or string | The group's id or name. |

Returns the [HotkeysInfo](#hotkeysinfo) as it is now.

```sh
weirctl raw remove_hotkey_group '{"group": "Streaming"}'
```

```python
mixer.call("remove_hotkey_group", group="Streaming")
```

```js
await mixer.call("remove_hotkey_group", { group: "Streaming" });
```

#### `move_hotkey_group`

Move a group of hotkeys to another place among the groups.

| Parameter | Type | |
|---|---|---|
| `group` | number or string | The group's id or name. |
| `index` | number | Its place among the groups, counting from 0. Past the end means last. |

Returns the [HotkeysInfo](#hotkeysinfo) as it is now.

```sh
weirctl raw move_hotkey_group '{"group": "Streaming", "index": 0}'
```

```python
mixer.call("add_hotkey_group", name="Games")
mixer.call("move_hotkey_group", group="Games", index=0)
```

```js
await mixer.call("move_hotkey_group", { group: "Streaming", index: 0 });
```

#### `open_shortcut_settings`

Open the desktop's shortcut settings at Weir's hotkeys, where people change
their keys and give them more. Takes no parameters. Returns `null`, or an
error unless [KeysStatus](#keysstatus) says `configurable`.

<!-- not tested: needs a desktop that can open its settings -->
```sh
weirctl raw open_shortcut_settings
```

<!-- not tested: needs a desktop that can open its settings -->
```python
if mixer.call("list_hotkeys")["keys"].get("configurable"):
    mixer.call("open_shortcut_settings")
```

<!-- not tested: needs a desktop that can open its settings -->
```js
await mixer.call("open_shortcut_settings");
```

### Settings and the window

#### `set_settings`

Change the daemon's settings. Only the fields you give change.

| Parameter | Type | |
|---|---|---|
| `solo` | `"exclusive"` or `{"cue": bus}`, *optional* | What soloing a strip does. `"exclusive"`: every other strip goes silent, in every mix. `{"cue": "A1"}`: only that bus's mix changes, so you can check a strip in your headphones without the stream hearing it. |
| `sample_rate` | number, *optional* | Hold PipeWire at this rate while Weir runs, 8000 to 384000 Hz, or `0` to let PipeWire decide. Noise suppression only works at 48000. |
| `quantum` | number, *optional* | Hold PipeWire at this buffer size while Weir runs, 16 to 8192 frames, or `0` to let PipeWire decide. Smaller means less delay and more CPU. |
| `meter_rate_hz` | number, *optional* | How many `meters` notifications a second, 1 to 120. |
| `startup` | string, *optional* | What the daemon does about the window when it starts: `window`, `minimized` or `tray_only`. |
| `start_at_login` | boolean or `"toggle"`, *optional* | Start Weir's service when you log in, or stop doing so, from the next login; the service running now carries on either way. An error where [Settings](#settings) has no `start_at_login`. |
| `tray` | boolean, *optional* | Show the tray icon. Takes effect when the daemon next starts. |
| `tray_icon` | string, *optional* | `color`, or `one_color` to match the panel like other tray icons. |

Returns the [Settings](#settings) as they are now.

```sh
weirctl raw set_settings '{"solo": {"cue": "A1"}}'
weirctl raw set_settings '{"sample_rate": 48000, "quantum": 256}'
weirctl raw set_settings '{"sample_rate": 0, "quantum": 0}'
weirctl raw set_settings '{"startup": "tray_only"}'
```

<!-- not tested: changes how the computer running the check starts up -->
```sh
weirctl raw set_settings '{"start_at_login": true}'
```

```python
mixer.call("set_settings", solo="exclusive")
```

```js
await mixer.call("set_settings", { meter_rate_hz: 60 });
```

#### `show_window`

Bring the mixer window to the front, or open it if it is not open. Takes
no parameters. Returns `null`.

<!-- not tested: needs a display -->
```sh
weirctl raw show_window
```

<!-- not tested: needs a display -->
```python
mixer.call("show_window")
```

<!-- not tested: needs a display -->
```js
await mixer.call("show_window");
```

## Notifications

The daemon sends these to connections that [subscribed](#subscribe) to
their topic, whenever something changes, whoever changed it. Each carries
the whole of what it is about, not the difference, so a client can simply
replace its copy.

| Notification | Topic | `params` | Sent when |
|---|---|---|---|
| `state_changed` | `state` | [MixerState](#mixerstate) | Anything about the strips and buses changed. |
| `eq_presets_changed` | `state` | list of [EqPreset](#eqpreset) | A preset was saved or deleted. |
| `history_changed` | `state` | [HistoryInfo](#historyinfo) | A change was made, undone or redone. |
| `app_rules_changed` | `state` | list of [AppRule](#apprule) | The application rules changed. |
| `library_changed` | `state` | [Library](#library) | A scene or setup was saved, loaded or deleted. |
| `meters` | `meters` | [Meters](#meters) | 30 times a second, or as often as `meter_rate_hz` says. |
| `devices_changed` | `devices` | list of [DeviceInfo](#deviceinfo) | A device was plugged in or out. |
| `system_volumes_changed` | `devices` | [SystemVolumes](#systemvolumes) | The system's volume on one of Weir's devices changed. |
| `inserts_changed` | `devices` | list of [InsertStatus](#insertstatus) | A program started or stopped playing into a "from effects" device, or external effects were switched on or off. |
| `apps_changed` | `apps` | list of [AppStream](#appstream) | An application started or stopped playing, moved, or its volume changed. |
| `engine_changed` | `engine` | [EngineStatus](#enginestatus) | The engine connected, stopped, or changed rate or buffer size. |
| `settings_changed` | `settings` | [Settings](#settings) | A setting changed. |
| `hotkeys_changed` | `hotkeys` | [HotkeysInfo](#hotkeysinfo) | A hotkey was added, changed or removed, the desktop gave them other keys, or something went wrong with one, or came right. |
| `spectrum` | see [`watch_spectrum`](#watch_spectrum) | [Spectrum](#spectrum) | 30 times a second for each strip or bus watched. |
| `show_window` | `window` | none | The mixer window should come to the front. |
| `quit` | `window` | none | The daemon is stopping, or another window is already open; the window should close. |

For example, after someone mutes the Mic strip:

```json
{"jsonrpc": "2.0", "method": "state_changed", "params": {"strips": [{"id": 1, "name": "Mic", "mute": true, "...": "..."}, "..."], "buses": ["..."]}}
```

The `window` topic is only for the mixer window: subscribing to it is how
the daemon knows a window is open, so that the tray raises it rather than
starting another. Subscribing with an empty list of topics does not include
it.

## Types

What the daemon sends and accepts, field by field. To keep messages small,
fields at their default are often left out of what the daemon sends: an
effect that has never been touched, a `sends` map with every level at 0 dB,
an `upmix` that is `off`. Treat a missing field as its default.

### FullState

What [`get_state`](#get_state) returns.

| Field | Type | |
|---|---|---|
| `mixer` | [MixerState](#mixerstate) | The strips and buses. |
| `devices` | list of [DeviceInfo](#deviceinfo) | The devices strips and buses can use. |
| `apps` | list of [AppStream](#appstream) | The applications playing. |
| `engine` | [EngineStatus](#enginestatus) | How the audio engine is doing. |
| `settings` | [Settings](#settings) | The daemon's settings. |
| `eq_presets` | list of [EqPreset](#eqpreset) | Every equalizer preset, built-in first. |
| `app_rules` | list of [AppRule](#apprule) | Where applications go when they start. |
| `library` | [Library](#library) | The saved scenes and setups. |
| `system_volumes` | [SystemVolumes](#systemvolumes) | The system's volume on Weir's devices. |
| `inserts` | list of [InsertStatus](#insertstatus) | Whether each strip's and bus's external effects are connected, for those that have them on. Left out when none do. |
| `hotkeys` | [HotkeysInfo](#hotkeysinfo) | The hotkeys, and how keys reach Weir. |

### MixerState

`{"strips": [Strip, ...], "buses": [Bus, ...]}`, each in the order the
mixer shows them, left to right.

### Strip

```json
{
  "id": 2, "name": "Music", "kind": "virtual", "layout": "stereo",
  "gain_db": -6.0, "mute": false, "solo": false, "pan": 0.0,
  "routes": [1, 3], "sends": {"3": -6.0}, "color": "#3CBEAF",
  "gate": {"enabled": true, "threshold_db": -45.0, "range_db": -90.0,
           "attack_ms": 2.0, "hold_ms": 200.0, "release_ms": 150.0}
}
```

| Field | Type | |
|---|---|---|
| `id` | number | Its id. |
| `name` | string | Its name. A virtual strip's device is called "*name* (Weir)", with any `:` in the name made `-`, since patchbays take `:` for the end of a program's name. |
| `kind` | string | `virtual`, a device applications play into, or `hardware`, which captures from `device`. |
| `layout` | [layout](#layout) | Its channels. |
| `gain_db` | number | The fader, -60 (silent) to +12 dB. |
| `mute`, `solo` | boolean | |
| `pan` | number | -1 (left) to 1 (right). |
| `routes` | list of numbers | The ids of the buses it is sent to. |
| `sends` | object | Its level in individual buses' mixes, on top of the fader, by bus id: `{"3": -6.0}`. Buses not listed are at 0 dB. A level is kept while its route is off. |
| `upmix` | string | See [Upmix](#upmix). `off` when left out. |
| `subwoofer` | boolean | Its bass also plays on the subwoofer of buses that have one: everything below 120 Hz, mixed to one channel. No effect on a strip with a subwoofer channel of its own. `false` when left out. |
| `device` | string | A hardware strip's device, by `name`, when it has one. |
| `color` | string | `"#RRGGBB"`, when it has one. |
| `denoise`, `gate`, `eq`, `compressor` | object | Its effects, in the order they work on the sound, all before the fader: [Denoise](#denoise), [Gate](#gate), [Equalizer](#equalizer), [Compressor](#compressor). |
| `ducking` | object | [Ducking](#ducking), which turns the strip down in some mixes while others are heard, after the fader. |
| `insert` | object | Its [external effects](#insert), which can go anywhere among the effects and the fader. |

### Bus

```json
{
  "id": 3, "name": "Stream Mic", "kind": "virtual", "layout": "stereo",
  "gain_db": 0.0, "mute": false, "mono": false,
  "limiter": {"enabled": true, "ceiling_db": -1.0, "release_ms": 300.0}
}
```

| Field | Type | |
|---|---|---|
| `id` | number | Its id. |
| `name` | string | Its name. A virtual bus's microphone is called "*name* (Weir)", with any `:` made `-` as for strips. |
| `kind` | string | `hardware`, which plays to `device`, or `virtual`, a virtual microphone. |
| `layout` | [layout](#layout) | Its channels. |
| `gain_db` | number | The fader, -60 (silent) to +12 dB. |
| `mute` | boolean | |
| `mono` | boolean | Every channel folded into one, on all its speakers. |
| `delay_ms` | number | How long the bus holds its output back, 0 to 500. Left out when it is 0, which is what a reader takes a missing one for. |
| `device` | string | A hardware bus's device, by `name`, when it has one. |
| `color` | string | `"#RRGGBB"`, when it has one. |
| `eq` | object | Its [Equalizer](#equalizer), before the fader. |
| `limiter` | object | Its safety [Limiter](#limiter), after the fader. |
| `downmix` | object | How it plays channels it has no speaker for: [Downmix](#downmix). |
| `insert` | object | Its [external effects](#insert). |

A bus's label, `A1` or `B2`, is not stored: it is the bus's place among
the buses of its kind, `A` for hardware and `B` for virtual, in the order
of `buses`.

### Layout

A strip's or bus's channels: `"mono"`, `"stereo"`, `"quad"`,
`"surround_5_1"` or `"surround_7_1"` (`"5.1"` and `"7.1"` are accepted
too), or any list of speaker positions as `{"custom": ["FL", "FR", "LFE"]}`.
The positions are `MONO`, `FL`, `FR` (front left and right), `FC` (center),
`LFE` (subwoofer), `RL`, `RR` (rear) and `SL`, `SR` (side). Up to 16
channels.

### Denoise

Noise suppression for speech, with RNNoise. It takes out steady background
noise such as fans and hum, and is trained on voices, so it does not suit
music. It delays its strip by 20 ms, and only works when PipeWire runs at
48000 Hz; at other rates the sound passes through unchanged.

| Field | Default | |
|---|---|---|
| `enabled` | `false` | |
| `amount` | `1` | How much of the cleaned sound to use, 0 to 1. Lower mixes some of the original back in, which sounds more natural and leaves more noise. |

### Gate

Silences a strip while it is quieter than a threshold, such as a
microphone while nobody talks. It closes 4 dB below the threshold rather
than at it, so a level hovering around it does not make it flutter.

| Field | Default | |
|---|---|---|
| `enabled` | `false` | |
| `threshold_db` | `-45` | It opens above this level, -90 to 0 dB. |
| `range_db` | `-90` | How far it turns the strip down while closed, -90 (silence) to 0 dB. |
| `attack_ms` | `2` | How quickly it opens, 0.1 to 100 ms. |
| `hold_ms` | `200` | How long it stays open after the level drops, 0 to 2000 ms. |
| `release_ms` | `150` | How gradually it closes after that, 5 to 3000 ms. |

### Compressor

Evens out a strip's level: above a threshold it turns the strip down, by a
ratio, then lifts the whole strip back up. Quiet words become easier to
hear and loud ones stop blasting.

| Field | Default | |
|---|---|---|
| `enabled` | `false` | |
| `threshold_db` | `-24` | It works above this level, -60 to 0 dB. |
| `ratio` | `4` | 1 to 20. At 4, every 4 dB over the threshold comes out as 1 dB over. |
| `attack_ms` | `10` | How quickly it turns down, 0.1 to 200 ms. |
| `release_ms` | `150` | How quickly it lets go, 10 to 3000 ms. |
| `auto_makeup` | `true` | Work out the lift from the threshold and ratio: half of what a full-scale sound is turned down by, `-threshold_db × (1 − 1/ratio) / 2`, to the nearest 0.5 dB. |
| `makeup_db` | `0` | The lift when `auto_makeup` is off, 0 to 24 dB. |

The curve bends smoothly over 6 dB around the threshold (a soft knee).
`Compressor::output_db` in the protocol crate computes it exactly as the
engine does, for drawing it.

### Ducking

Turns a strip down in some mixes while other strips are heard: music that
gets quieter on the stream while you talk, for example.

| Field | Default | |
|---|---|---|
| `enabled` | `false` | |
| `triggers` | `[]` | The strips that turn this one down, by id. A strip counts as heard while its level, after its fader, is above `threshold_db`, and its gate, if on, is open. |
| `amount_db` | `12` | How far it turns down, 0 to 60 dB. |
| `buses` | `[]` | The buses whose mixes it happens in, by id. Empty means every bus. |
| `threshold_db` | `-40` | -80 to 0 dB. |
| `attack_ms` | `50` | How quickly it goes down, 1 to 2000 ms. |
| `hold_ms` | `300` | How long it stays down once the triggers are quiet, 0 to 5000 ms. |
| `release_ms` | `800` | How quickly it comes back, 10 to 10000 ms. |

When setting it, `triggers` and `buses` take names as well as ids, and
replace the whole list. Removing a strip or bus takes it out of both.

### Equalizer

`{"enabled": false, "bands": []}` by default: up to 16 [bands](#eqband),
which work one after the other. Switching it off keeps the bands. When
changing it, `bands` replaces all of them at once.

### EqBand

```json
{"kind": "peak", "freq_hz": 3000.0, "gain_db": 2.5, "q": 1.0, "enabled": true}
```

| Field | Default | |
|---|---|---|
| `kind` | `peak` | Its shape: `peak` (a bell), `low_shelf`, `high_shelf`, `low_pass`, `high_pass`, `notch` or `band_pass`. |
| `freq_hz` | required | Its center or corner frequency, 20 to 20000 Hz. |
| `gain_db` | `0` | Boost or cut, -24 to +24 dB. Only bells and shelves use it. |
| `q` | `0.707` | Width for bells, notches and band passes (higher is narrower); resonance for shelves and pass filters, where 0.707 is neutral. 0.1 to 20. |
| `enabled` | `true` | A band switched off keeps its settings. |

`EqBand::coefs(rate).response_db(freq, rate)` in the protocol crate gives
exactly the curve the engine applies, for drawing it.

### EqPreset

`{"name": "Voice: clarity", "bands": [EqBand, ...], "builtin": true}`.
Built-in presets cannot be changed or deleted.

### Limiter

A bus's safety limiter: whatever comes in, the bus never goes over its
ceiling, so nothing that records or hears it clips. It looks 1.5 ms ahead,
and delays the bus that much while it is on.

| Field | Default | |
|---|---|---|
| `enabled` | `false` | New virtual buses start with it on. |
| `ceiling_db` | `-1` | The level the bus never goes over, -60 to +12 dB. |
| `release_ms` | `300` | How long the bus takes to come back up after being turned down, 10 to 2000 ms. |

### Insert

External effects: a point in a strip's or bus's chain where its sound
leaves Weir for another program, such as [Carla](https://kx.studio/Applications:Carla)
or EasyEffects, and comes back. While they are on, Weir makes two devices
for them: "*name* to effects (Weir)", a microphone the effects program
records from, and "*name* from effects (Weir)", an output it plays into.
Link them to the program in its own settings or in a patchbay such as
Carla's or qpwgraph.

Weir links "from effects" on into a node of its own, "Weir effects return"
(`weir.effects-return`), which exists while any external effects are on
and has a port per channel of each, such as `from_effects_strip_2_FL`. So
a patchbay shows a line, from the engine, "Weir Engine", through the
effects program and on to the return node. A program can also be linked
straight into the return node's ports; that counts as connected too.

```json
{"enabled": true, "position": "before_compressor", "fallback": "pass_through"}
```

| Field | Default | |
|---|---|---|
| `enabled` | `false` | Whether the sound goes out and back. Switching it on or off makes or removes the two devices. |
| `position` | `before_fader` | Where in the chain, from the lists below. |
| `fallback` | `pass_through` | What carries on while nothing plays into "from effects": `pass_through`, the sound as it went out, as if the external effects were off, or `silence`. |

A strip's places, in the order of its chain: `before_denoise`,
`before_gate`, `before_eq`, `before_compressor`, `before_fader`,
`after_fader`. A bus's: `before_eq` (straight after its mix),
`before_fader`, `before_limiter`, `after_limiter`. Asking for a place the
strip or bus does not have is an error. After a strip's fader, the effects
hear it as loud as it is in the mix, and what comes back goes into the
buses' mixes as it is; ducking and the send levels still apply.

The round trip adds one PipeWire cycle, a few milliseconds, besides
whatever the effects program takes. Weir switches between what comes back
and the fallback as programs connect and go, fading over 10 ms.
[InsertStatus](#insertstatus) says which it is using.

### InsertStatus

`{"target": {"strip": 1}, "connected": true}`: whether something plays
into the "from effects" device of a strip's or bus's
[external effects](#insert). While not connected, the strip or bus plays
its `fallback`.

### Downmix

How a bus plays the channels of a strip it has no speaker for, such as a
5.1 film on stereo headphones.

| Field | Default | |
|---|---|---|
| `method` | `standard` | See below. |
| `center_db` | `-3` | How loud the center channel, which carries most dialog, is in each front speaker, -60 to 0 dB. -3, 0, -4.5 and -6 are the usual choices; louder makes speech clearer. |
| `surround_db` | `-3` | How loud each surround channel is in the front speaker on its side, -60 (left out) to 0 dB. `standard` only. |
| `lfe` | `false` | Keep the subwoofer channel, at -3 dB in each front speaker. Standard downmixes leave it out. |

The methods:

* `standard` (ITU-R BS.775, "Lo/Ro"): the center into both fronts, each
  surround into the front on its side.
* `matrix` ("Lt/Rt", the levels of Dolby Pro Logic II): the surrounds go
  into both fronts with opposite polarity, so a receiver that decodes
  surround from stereo can recover them.
* `front_only`, `center_only`, `lfe_only`, `surround_only`: play one part
  of a strip, on the bus's front pair, and nothing else. For building
  surround out of stereo devices, one bus for each part.

A strip with more channels than a bus has speakers for always plays the
rest somewhere: a 7.1 strip on a 5.1 bus puts each side and rear pair into
the one pair at -3 dB, and a mono bus gets the stereo downmix, averaged.

### Upmix

How a strip plays on a bus with speakers it has no channel for, such as
stereo music on 5.1 speakers. It only ever fills speakers nothing else
plays.

* `off`: only the speakers it has channels for.
* `center`: the center speaker too, with the average of left and right.
* `all_channel_stereo`: every speaker, its left channel on each left
  speaker and its right channel on each right one, plus the center.
* `passive_surround`: passive surround decoding. The center as `center`,
  and on the surround speakers the difference between left and right,
  low-passed at 7 kHz and 12 ms late, which is where much of a stereo
  recording's ambience lives.

### StripOrBus

`{"strip": 2}` or `{"bus": 1}`. When sending, names and labels work too:
`{"strip": "Mic"}`, `{"bus": "B1"}`.

### DeviceInfo

```json
{"id": 45, "name": "alsa_input.usb-Blue_Yeti-00.analog-stereo",
 "description": "Yeti Stereo Microphone", "kind": "source", "channels": ["FL", "FR"]}
```

| Field | |
|---|---|
| `id` | PipeWire's id for it, which changes when it is plugged in again. |
| `name` | Its PipeWire `node.name`, which does not. Use this to choose a device. A speaker's monitor, which hears what it plays, is its name with `.monitor` after it. |
| `description` | Its name for people. |
| `kind` | `source` (microphones and other inputs, for hardware strips) or `sink` (outputs, for hardware buses). |
| `channels` | Its channels' positions. |

### AppStream

```json
{"id": 87, "name": "Firefox", "binary": "firefox", "media_name": "Rick Astley - Never Gonna Give You Up",
 "target": "weir.input.3", "strip": 3, "volume_db": -6.0, "mute": false}
```

| Field | |
|---|---|
| `id` | PipeWire's id for the stream. It is given to the next stream once this one ends, so tell applications apart by name too. |
| `name` | The application's name. |
| `binary` | The program, when PipeWire knows it. |
| `media_name` | What it is playing, when it says. |
| `target` | The PipeWire `node.name` it plays to. |
| `strip` | The strip it plays into, when it plays into one of Weir's. |
| `volume_db` | Its own volume, as the system's volume control shows it, once it has reported one. |
| `mute` | Its own mute. |

### AppRule

`{"app": "Spotify", "strip": 2}`: when an application whose `name` or
`binary` is `app` (ignoring case) starts playing, move it to strip 2. A
rule without `strip` leaves the application wherever it goes by itself.
When setting rules, `strip` takes a name as well as an id.

### Settings

```json
{"meter_rate_hz": 30, "startup": "window", "start_at_login": true, "tray": true,
 "tray_icon": "color", "solo": "exclusive", "sample_rate": 48000, "quantum": 256}
```

| Field | |
|---|---|
| `solo` | `"exclusive"`: a soloed strip silences every other strip, in every mix. `{"cue": 1}`: soloing only changes bus 1's mix, which plays just the soloed strips. |
| `sample_rate`, `quantum` | The rate and buffer size PipeWire is held at while Weir runs. Left out when PipeWire decides. [EngineStatus](#enginestatus) says what it is actually running at. |
| `meter_rate_hz` | How many `meters` notifications a second. |
| `startup` | `window`, `minimized` or `tray_only`: whether the daemon opens the mixer window when it starts. |
| `start_at_login` | Whether Weir's service starts when you log in. Left out where that cannot be set: Weir's service is not installed, as when running from a build folder, or systemd is not running. systemd keeps this rather than the configuration file; the daemon reads it back when it starts and after changing it, so a change made with `systemctl --user enable weir` shows after the daemon restarts. |
| `tray` | Whether it shows a tray icon. |
| `tray_icon` | `color` or `one_color`. |

### EngineStatus

```json
{"connected": true, "state": "streaming", "sample_rate": 48000, "quantum": 1024, "node_id": 34}
```

`state` is `streaming` when all is well, `paused` when nothing is connected
to it, and otherwise `connecting`, `unconnected`, `error` (with the reason
in `error`) or `disabled` (a daemon started without its engine). A quantum
of 1024 frames at 48000 Hz is 21 ms of delay.

### Meters

```json
{
  "strips": {"1": [-24.5], "2": [-12.0, -13.1]},
  "buses": {"1": [-10.2, -11.0], "3": [-20.3, -20.3]},
  "gates": {"1": {"level_db": -52.0, "reduction_db": -100.0}},
  "compressors": {"1": {"level_db": -14.0, "reduction_db": -6.5}},
  "ducking": {"2": -11.8},
  "limiters": {"3": 0.0}
}
```

Every level is the peak since the previous meter message, in dB below full
scale, per channel, with -100 for silence.

| Field | |
|---|---|
| `strips` | Each strip's level after its effects and fader, by id. It does not include ducking, which only happens in the mixes. |
| `buses` | Each bus's output, after its limiter. |
| `gates` | Strips with their gate on: the level going in (`level_db`), to compare with the threshold, and how far the gate is turning the strip down right now (`reduction_db`: 0 while open, -100 when closed all the way). |
| `compressors` | Strips with their compressor on: the level going in, and the most it turned the strip down since the previous message, before the lift. |
| `ducking` | Strips with ducking on: the most it turned the strip down, in dB (0 or less). |
| `limiters` | Buses with their limiter on: the most it turned the bus down. |

The last four are left out when empty.

### Spectrum

`{"target": StripOrBus, "input_db": [...], "output_db": [...]}`: what goes
into a strip's or bus's equalizer and what comes out, for drawing an
analyzer. Each list has 192 levels, at frequencies spaced evenly in octaves
from 20 Hz to 20 kHz: point `i` is at `20 × 1000^(i / 191)` Hz. A
full-scale sine reads 0 dB and silence -120. The levels lean up by 4.5 dB
an octave around 1 kHz, so pink noise, which sounds even, reads flat. They
are smoothed for drawing already: a level rises at once and falls at 45 dB
a second. For a strip, the input is after noise suppression and the gate.

### SystemVolumes

`{"strips": {"2": SystemVolume}, "buses": {"3": SystemVolume}}`: the volume
the system's volume control (KDE's, pavucontrol) has set on each of Weir's
virtual devices, by the strip or bus it belongs to. A SystemVolume is
`{"volume_db": -9.3, "mute": false}`, for its loudest channel.

Weir reports these and never changes them; they belong to the system.
Volume controls show them on a cubic scale: percent is
`cbrt(10^(volume_db / 20)) × 100`, so -9.3 dB shows as 70%.

### Library

`{"scenes": ["Late night", "Streaming"], "setups": ["Desk"], "scene": "Streaming", "setup": "Desk"}`:
the saved scenes and setups, and the one of each last loaded or saved, if
any.

### Hotkey

```json
{"id": 2, "name": "Push to talk", "keys": ["F9"],
 "steps": [{"method": "set_strip", "params": {"id": 1, "mute": false}}],
 "on_release": "restore"}
```

The fields are those of [`set_hotkey`](#set_hotkey). Fields at their
default are left out: `enabled` when `true`, `group` when `0`, `each_press`
when `all`, `on_release` when `nothing`.

### HotkeyGroup

```json
{"id": 1, "name": "Streaming", "enabled": false}
```

| Field | Type | |
|---|---|---|
| `id` | number | Given by the daemon. |
| `name` | string | Unique among groups, ignoring case. |
| `enabled` | boolean, *optional* | Whether its hotkeys' keys work; left out when `true`. |

### HotkeyStep

| Field | Type | |
|---|---|---|
| `method` | string | The request, such as `set_strip` or `load_scene`. |
| `params` | object, *optional* | Its parameters, as in the request. |
| `over_ms` | number, *optional* | Fade over this many milliseconds, up to 60000: for `gain_db` in `set_strip` and `set_bus`, and `level_db` in `set_route`. Anything else in the step changes at once. |

### Keys

Any of `Ctrl`, `Alt`, `Shift` and `Super` (the Windows key), then one key,
joined by `+`: `Ctrl+Alt+M`, `Super+F1`, `Pause`. Upper or lower case, and
`Control`, `Win` or `Meta` work too; the daemon writes them back the usual
way.

Keys: `A` to `Z`, `0` to `9`, `F1` to `F24`, `Space`, `Enter`, `Tab`,
`Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`,
`Up`, `Down`, `Left`, `Right`, `Escape`, `Pause`, `Print`, `ScrollLock`,
punctuation by name (`Minus`, `Equal`, `Comma`, `Period`, `Slash`,
`Semicolon`, `Apostrophe`, `BracketLeft`, `BracketRight`, `Backslash`,
`Grave`), the number pad (`Num0` to `Num9`, `NumPlus`, `NumMinus`,
`NumMultiply`, `NumDivide`, `NumEnter`, `NumPeriod`), and media keys
(`Mute`, `VolumeUp`, `VolumeDown`, `MicMute`, `Play`, `Stop`, `PreviousTrack`,
`NextTrack`).

Keys that type or move around, letters to arrows, need Ctrl, Alt or
Super, so a hotkey never takes them from what you are typing. Function
keys, media keys, `Pause`, `Print` and `ScrollLock` can be on their own.

### HotkeysInfo

| Field | Type | |
|---|---|---|
| `hotkeys` | list of [Hotkey](#hotkey) | Every hotkey, in their order in the list. Those in a group are listed with it, in the same order. |
| `groups` | list of [HotkeyGroup](#hotkeygroup), *optional* | Every group, in their order in the list, after the hotkeys in no group. Left out when there are none. |
| `keys` | [KeysStatus](#keysstatus) | How keys reach Weir on this desktop. |
| `problems` | list of `{"hotkey": id, "problem": string}` | What is wrong with any of them, in sentences for people: keys another program has, a strip that was removed. Left out when there is nothing wrong. |

### KeysStatus

| Field | Type | |
|---|---|---|
| `method` | string | `desktop`: the desktop looks after the keys, through the XDG desktop portal's global shortcuts (KDE Plasma, GNOME 48 and newer, Hyprland). Each hotkey with keys is one entry in the desktop's shortcut settings, suggesting the first of its `keys`, where people change them and add more (see `settable`). `x11`: Weir watches the keys itself, on an X11 desktop. `unavailable`: neither, so hotkeys are pressed only by name, for instance from a shortcut of the desktop's own running `weirctl hotkey run NAME`. `starting`: not known yet, as at login before the desktop is up. |
| `message` | string | What that means, in a sentence to show people. |
| `assigned` | object, *optional* | With `desktop`: the keys each hotkey really has, by id, as the desktop writes them, such as `{"1": ["F9", "Ctrl+Alt+I"]}`; an empty list for one it has given none. They can differ from the hotkey's `keys`, since they are changed and added to in the desktop's settings. A hotkey switched off that the desktop never had is left out. |
| `configurable` | boolean, *optional* | With `desktop`: `true` when [`open_shortcut_settings`](#open_shortcut_settings) can open the desktop's settings at Weir's hotkeys (KDE Plasma 6.5 and newer). |
| `settable` | boolean, *optional* | With `desktop`: `true` when Weir sets the desktop's keys itself (KDE Plasma). A hotkey's `keys` all work, keys changed in the desktop's settings come back into `keys`, and a key another program has is left out, with a problem saying which. Otherwise only the first of a hotkey's `keys` is suggested. |

### HistoryInfo

```json
{"undo": [{"label": "Music fader", "at_ms": 1790000000000}], "redo": []}
```

What `undo` and `redo` would take back or bring back, most recent first, up
to 25 of each: a label for people, and when it happened, in milliseconds
since 1970.

## Errors

A request that fails gets an `error` in place of a `result`, with a `code`
saying what kind of problem it was and a `message` saying what exactly, in
words meant for people. The message is fine to show to a user as it is.

```json
{"jsonrpc": "2.0", "id": 7, "error": {"code": -32000, "message": "no strip called 'Musci'"}}
```

| Code | Meaning | For example |
|---|---|---|
| -32700 | The line was not JSON. | `{nope` |
| -32600 | The JSON was not a request. | A request without a `method`, or an empty batch. |
| -32601 | There is no such method. | `unknown method 'set_strips'` |
| -32602 | The parameters are wrong: missing, of the wrong type, or not allowed. | `invalid type: string "loud", expected f32`, `name is too long (40 characters max)` |
| -32000 | The request was understood, but cannot be done as things are. | `no strip called 'Musci'`, `the name 'Mic' is already in use`, `nothing to undo`, `apps can only be moved to virtual strips` |
| -32001 | The audio engine could not do it. | Moving an application when PipeWire refuses. |
| -32603 | Something went wrong inside the daemon. | Should not happen; please report it. |

The example clients raise these as `WeirError` (Python) or reject with a
`WeirError` (JavaScript), with `code` and `message`:

```python
try:
    mixer.call("set_strip", id="Musci", mute=True)
except weir.WeirError as e:
    print(f"Weir said no: {e.message}")
```

```js
try {
  await mixer.call("set_strip", { id: "Musci", mute: true });
} catch (e) {
  console.log(`Weir said no: ${e.message}`);
}
```

## Recipes

### A mute button that shows whether you are muted

Toggle on press; light the key from notifications, so it stays right when
the mute is changed anywhere else.

<!-- runs until stopped -->
```js
const mixer = await weir.connect();
const showMute = (strip) => console.log(strip.mute ? "MUTED" : "live"); // light the key here

mixer.on("state_changed", (state) => showMute(weir.find(state.strips, "Mic")));
await mixer.subscribe(["state"]);
showMute(weir.find((await mixer.call("get_state")).mixer.strips, "Mic"));

// When the key is pressed:
await mixer.call("set_strip", { id: "Mic", mute: "toggle" });
```

### A volume knob

Each detent is a step. Sent without an id, so the daemon does not answer
and a fast turn is not held up waiting.

```python
def knob_turned(detents):
    mixer.send("set_strip", id="Music", gain_delta_db=detents * 1.0)

knob_turned(+2)
```

A turn lasting a while is one step in the undo history, not one per
detent.

### Push to talk

Unmuted while the key is held, as a hotkey: Weir watches the key, and
letting go puts the mute back as it was.

```sh
weirctl raw set_hotkey '{"name": "Talk", "keys": ["F9"], "steps": [{"method": "set_strip", "params": {"id": "Mic", "mute": false}}], "on_release": "restore"}'
```

A button on something else, such as a Stream Deck, can hold the same
hotkey:

```python
def button_down():
    mixer.call("press_hotkey", hotkey="Talk")

def button_up():
    mixer.call("release_hotkey", hotkey="Talk")
```

For push to mute, `"mute": true`. A gate (`"gate": {"enabled": true}`)
often does the job without any key at all.

### Level meters on keys or a display

Subscribe to meters at the rate you can draw, and take the loudest channel.
Nothing is lost at a lower rate: each message holds the peaks since the one
before.

<!-- runs until stopped -->
```python
mixer.subscribe("meters", meter_rate_hz=10)
for method, params in mixer.events():
    if method == "meters":
        mic = max(params["strips"]["1"])
        print(f"Mic {mic:.0f} dB")
```

A full level meter for the terminal is
[`examples/python/level_meter.py`](../examples/python/level_meter.py).

### Music that ducks while you talk

Turn Music down by 15 dB in the stream's mix whenever the microphone is
heard; your headphones keep the full level.

```sh
weirctl raw set_strip '{"id": "Music", "ducking": {"enabled": true, "triggers": ["Mic"], "amount_db": 15, "buses": ["B1"]}}'
```

The `ducking` meter says when it is happening, for a light on a key.

### Applications always in the right place

Rules move applications as they start, so Spotify always plays into
Music and a browser into Browser, whatever device they last used.

```sh
weirctl raw set_app_rules '{"rules": [{"app": "spotify", "strip": "Music"}, {"app": "firefox", "strip": "Browser"}]}'
```

### A scene per moment

Save how things sound for each thing you do, and switch with one request,
from a key or when a program starts:

```sh
weirctl raw save_scene '{"name": "Streaming"}'
weirctl raw load_scene '{"name": "Streaming"}'
```

[`examples/js/next_scene.js`](../examples/js/next_scene.js) steps through
the saved scenes, one per press.

### Several changes at once

A batch is one message and one answer, and the changes happen in order:

```python
mixer.batch(
    ("set_strip", {"id": "Mic", "mute": False}),
    ("set_route", {"strip": "Music", "bus": "B1", "enabled": False}),
    ("set_bus", {"id": "A1", "gain_db": -6}),
)
```
