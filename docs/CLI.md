# weirctl

`weirctl` controls Weir from a terminal, a script or a keyboard shortcut.
It can do everything the mixer window can, and needs the daemon to be
running, which it is whenever the window is open or the service is enabled.

```sh
weirctl state                          # everything at a glance
weirctl strip Mic --mute toggle        # mute or unmute the microphone
weirctl strip Music --gain-by -3       # music 3 dB quieter
weirctl route Music B1 off             # stop sending music to the stream
weirctl scene load Streaming           # everything as saved for streaming
```

## Contents

* [How it reads what you type](#how-it-reads-what-you-type)
* [Looking around](#looking-around): `status`, `state`, `devices`, `apps`
* [Strips](#strips): `strip`
* [Buses](#buses): `bus`
* [Routing](#routing): `route`
* [Adding, removing and moving](#adding-removing-and-moving)
* [Applications](#applications): `move-app`, `app-volume`, `rules`, `rule`, `unrule`
* [Equalizer presets](#equalizer-presets): `eq`
* [Scenes and setups](#scenes-and-setups): `scene`, `setup`
* [Undo](#undo): `undo`, `redo`, `history`
* [Hotkeys](#hotkeys): `hotkeys`, `hotkey`
* [Settings and the window](#settings-and-the-window): `settings`, `show`
* [Watching and anything else](#watching-and-anything-else): `watch`, `raw`
* [A mute key for any desktop](#a-mute-key-for-any-desktop)

## How it reads what you type

**Strips and buses** can be named by their name (`Music`, or `"Stream Mic"`
in quotes when it has a space) or their id (`2`), ignoring case. Buses also
answer to their label as the mixer shows it: `A1`, `A2`... for buses that
play to a device, `B1`, `B2`... for virtual microphones.

**Switches** such as `--mute` take `on`, `off` or `toggle` (and `yes`, `no`,
`true`, `false`, `1` and `0`).

**Levels** are in decibels: a fader goes from -60 (silent) to 12, and 0
leaves the sound as it is. Options ending in `-by` change a value by an
amount rather than setting it: `--gain-by -3` is 3 dB quieter than now.

Every command prints what it changed. With `--json` it prints the
daemon's answer as JSON instead, for scripts. `--socket PATH` talks to a
daemon listening somewhere other than `$XDG_RUNTIME_DIR/weir/control.sock`.

When something goes wrong, `weirctl` says what on its error output and
exits with status 1:

```
$ weirctl strip Musci --mute on
error: no strip called 'Musci'
```

`weirctl help` lists the commands, and `weirctl help strip` (or any other
command) lists its options.

## Looking around

| Command | Shows |
|---|---|
| `weirctl status` | Whether the engine is running, at what rate and buffer size, and how many strips, buses, devices and applications there are. |
| `weirctl state` | Every strip and bus: level, mute, solo, pan, effects, routes and device, and whether external effects are connected. |
| `weirctl devices` | The devices strips and buses can use, with the names `--device` takes. |
| `weirctl apps` | The applications playing, where, and at what volume. |

```sh
weirctl status
weirctl state
weirctl devices
weirctl apps
```

## Strips

### `weirctl strip STRIP [options]`

Changes a strip. Give as many options as you like; the rest stay as they
are. It prints the strip as it is afterwards.

**Level and pan**

| Option | |
|---|---|
| `--gain DB` | Set the fader, -60 to 12. |
| `--gain-by DB` | Move the fader by this much, such as `3` or `-3`. |
| `--pan N` | Left to right, -1 to 1; 0 is the middle. |
| `--mute on\|off\|toggle` | Silence it. |
| `--solo on\|off\|toggle` | Hear only soloed strips (see `settings --solo`). |

**What it is**

| Option | |
|---|---|
| `--name NAME` | Rename it. |
| `--color COLOR` | `orange`, `red`, `yellow`, `green`, `teal`, `blue`, `purple`, `pink`, `gray`, any `"#rrggbb"`, or `none`. |
| `--device NAME` | For a hardware strip, the device to capture from, by the name `weirctl devices` shows; `none` for none. |
| `--layout LAYOUT` | `mono`, `stereo`, `quad`, `5.1`, `7.1`, or the speakers in order, such as `"FL FR LFE"`. |
| `--upmix off\|center\|all\|passive` | How it plays on buses with more speakers than it has channels: its own speakers only, the center speaker too, every speaker (all-channel stereo), or passive surround decoding. |
| `--subwoofer on\|off` | Also play its bass on the subwoofer of buses that have one. |

**Effects**

| Option | |
|---|---|
| `--denoise on\|off` | Noise suppression, for voices. |
| `--denoise-amount PERCENT` | How much, 0 to 100. |
| `--gate on\|off` | The noise gate: silences the strip while it is quiet. |
| `--gate-threshold DB` | The gate opens above this level, -90 to 0. |
| `--gate-range DB` | How far it turns the strip down when closed, -90 (silence) to 0. |
| `--gate-attack MS`, `--gate-hold MS`, `--gate-release MS` | How quickly it opens, how long it stays open after the level drops, and how gradually it closes. |
| `--eq on\|off` | The equalizer. Switching it off keeps the bands; set them in the window or with `weirctl eq`. |
| `--comp on\|off` | The compressor, which evens out a voice. |
| `--comp-threshold DB` | It turns the strip down above this level, -60 to 0. |
| `--comp-ratio N` | How hard, 1 to 20: 4 means every 4 dB over comes out as 1 dB over. |
| `--comp-attack MS`, `--comp-release MS` | How quickly it turns down and lets go. |
| `--comp-lift DB\|auto` | How far it turns the strip back up afterwards, 0 to 24, or `auto` to work it out. |
| `--duck on\|off` | Ducking: turn this strip down while others are heard. |
| `--duck-when STRIPS` | The strips that duck it, separated by commas. |
| `--duck-by DB` | How far to turn it down. |
| `--duck-in BUSES` | The buses whose mixes it happens in, separated by commas, or `all`. |
| `--duck-threshold DB` | How loud a strip in `--duck-when` must be to count as heard. |

**External effects**

The strip's sound can go out to another program, such as Carla or
EasyEffects, and come back, at any point of its chain. While on, the strip
has two more devices: "*name* to effects (Weir)", which the program
records from, and "*name* from effects (Weir)", which it plays into.

| Option | |
|---|---|
| `--external-effects on\|off\|toggle` | Send the sound out and take it back. |
| `--external-effects-at PLACE` | Where: `before-denoise`, `before-gate`, `before-eq`, `before-compressor`, `before-fader` (the default) or `after-fader`. |
| `--external-effects-fallback pass\|silence` | What the strip plays while nothing comes back: its sound as if they were off (`pass`, the default), or nothing. |

```sh
weirctl strip Music --gain -6
weirctl strip Music --gain-by 2
weirctl strip Mic --mute toggle
weirctl strip Music --pan -0.3 --color teal
weirctl strip Browser --name Chat
weirctl strip Mic --device alsa_input.usb-Blue_Yeti-00.analog-stereo
weirctl strip Mic --denoise on --gate on --gate-threshold -40
weirctl strip Mic --comp on --comp-threshold -24 --comp-ratio 4 --comp-lift auto
weirctl strip Music --duck on --duck-when Mic --duck-in B1 --duck-by 12
weirctl strip Music --layout 5.1 --upmix all --subwoofer on
weirctl strip Mic --external-effects on --external-effects-at before-compressor
```

## Buses

### `weirctl bus BUS [options]`

Changes a bus, and prints it as it is afterwards.

| Option | |
|---|---|
| `--gain DB`, `--gain-by DB` | Set or move the fader. |
| `--mute on\|off\|toggle` | Silence it. |
| `--mono on\|off\|toggle` | Fold every channel into one. |
| `--delay MS` | Hold the bus's output back by this many milliseconds, 0 to 500, to line it up with a bus that plays later, such as a Bluetooth speaker. |
| `--delay-by MS` | Move the delay by this many milliseconds, such as 5 or -5. |
| `--name NAME`, `--color COLOR`, `--layout LAYOUT` | As for strips. |
| `--device NAME` | For a hardware bus, the device to play to; `none` for none. |
| `--eq on\|off` | The equalizer. |
| `--limiter on\|off` | The safety limiter, which keeps the bus from going over its ceiling. |
| `--limiter-ceiling DB` | The level it never goes over, -60 to 12. |
| `--downmix METHOD` | How it plays channels it has no speaker for: `standard`, `matrix`, or one part only: `front-only`, `center-only`, `lfe-only`, `surround-only`. |
| `--center-level DB` | The center channel's level in the downmix: `0`, `-3` (standard), `-4.5` or `-6`. |
| `--surround-level DB\|off` | The surround channels' level: `0`, `-3` (standard), `-6` or `off`. |
| `--keep-lfe on\|off` | Keep the subwoofer channel in the downmix. |
| `--external-effects on\|off\|toggle` | Send the mix out to another program and take it back, as for strips. |
| `--external-effects-at PLACE` | Where: `before-eq` (straight after the mix), `before-fader` (the default), `before-limiter` or `after-limiter`. |
| `--external-effects-fallback pass\|silence` | What the bus plays while nothing comes back. |

```sh
weirctl bus A1 --gain -3
weirctl bus Speakers --mute toggle
weirctl bus "Stream Mic" --limiter on --limiter-ceiling -1
weirctl bus A1 --delay 180      # wait for a Bluetooth speaker
weirctl bus A1 --delay-by -5    # a little less
weirctl bus A1 --downmix matrix --center-level -6
weirctl bus A2 --device alsa_output.pci-0000_00_1f.3.analog-stereo
weirctl bus "Stream Mic" --external-effects on --external-effects-at before-limiter
```

## Routing

### `weirctl route STRIP BUS [on|off|toggle] [--level DB] [--level-by DB]`

Sends a strip to a bus, stops it, or sets how loud it is in that bus's mix,
on top of its fader. With neither a switch nor a level, the route toggles.

```sh
weirctl route Music B1             # toggle
weirctl route Mic A1 on
weirctl route Music B1 --level -10 # quieter on the stream only
weirctl route Music B1 --level-by 3
```

## Adding, removing and moving

| Command | |
|---|---|
| `weirctl add-strip NAME --kind virtual\|hardware [--layout L] [--device D] [--route BUS]...` | Add a strip. A virtual one is a device applications can play into, called "*NAME* (Weir)". `--route` can be given once per bus to send it to. |
| `weirctl add-bus NAME --kind hardware\|virtual [--layout L] [--device D]` | Add a bus. A virtual one is a microphone other programs can record from. |
| `weirctl remove-strip STRIP`, `weirctl remove-bus BUS` | Remove one. `weirctl undo` brings it back. |
| `weirctl move-strip STRIP POSITION`, `weirctl move-bus BUS POSITION` | Move one, counting from 1 on the left. |

```sh
weirctl add-strip Game --kind virtual --layout 7.1 --route A1 --route B1
weirctl remove-strip Game
weirctl add-bus Recording --kind virtual
weirctl remove-bus Recording
weirctl move-strip Mic 4
weirctl move-bus Speakers 1
```

## Applications

| Command | |
|---|---|
| `weirctl move-app APP STRIP` | Move a playing application, by the id `weirctl apps` shows, to a virtual strip. |
| `weirctl app-volume APP [--gain DB] [--gain-by DB] [--mute on\|off\|toggle]` | Set an application's own volume, the one the system's volume control shows for it. |
| `weirctl rules` | List where applications go when they start playing. |
| `weirctl rule APP STRIP` | Always put an application on a strip when it starts playing, by the name `weirctl apps` shows or its program's name. `leave` in place of a strip leaves it alone. |
| `weirctl unrule APP` | Remove its rule. |

```sh
weirctl apps
weirctl move-app 87 Music
weirctl app-volume 87 --gain -6
weirctl app-volume 87 --mute toggle
weirctl rule Spotify Music
weirctl rules
weirctl unrule Spotify
```

Application ids come from PipeWire and change each time an application
starts, so look them up with `weirctl apps` first. For an application that
should always go to the same strip, a rule is simpler.

## Equalizer presets

| Command | |
|---|---|
| `weirctl eq presets` | List the presets, built-in ones first. |
| `weirctl eq show --strip S` or `--bus B` | Show a strip's or bus's equalizer bands. |
| `weirctl eq apply PRESET --strip S` or `--bus B` | Use a preset's bands, and switch the equalizer on. |
| `weirctl eq save NAME --strip S` or `--bus B` | Save a strip's or bus's bands as a preset of your own. |
| `weirctl eq delete NAME` | Delete a preset of your own. |

```sh
weirctl eq presets
weirctl eq apply "Voice: clarity" --strip Mic
weirctl eq show --strip Mic
weirctl eq save "My mic" --strip Mic
weirctl eq delete "My mic"
```

## Scenes and setups

A **scene** keeps the mix: levels, mutes, routes and effects. A **setup**
keeps the mixer itself: its strips and buses, their devices, names and
layouts. Both take the same commands:

| Command | |
|---|---|
| `weirctl scene list` | List them, marking the current one. |
| `weirctl scene save NAME` | Save how things are now, replacing one of that name. |
| `weirctl scene load NAME` | Bring one back. `weirctl undo` takes it back again. |
| `weirctl scene delete NAME` | Delete one. |

```sh
weirctl scene save Streaming
weirctl scene load Streaming
weirctl setup save Desk
weirctl setup list
```

## Undo

Every change to the mix, from the window, `weirctl` or anything else, can
be undone.

| Command | |
|---|---|
| `weirctl undo [--steps N]` | Take back the last change, or the last N. |
| `weirctl redo [--steps N]` | Bring back what was undone. |
| `weirctl history` | List what can be undone and redone. |

```sh
weirctl strip Music --gain -20
weirctl undo
weirctl history
```

## Hotkeys

A hotkey is keys and what they do: one or more steps, each a request of
the [control protocol](API.md#hotkeys), so a hotkey can do anything
`weirctl raw` can. Weir watches for the keys whichever window is in front.

| Command | |
|---|---|
| `weirctl hotkeys` | List them, what each does, how keys reach Weir on this desktop, and anything wrong. |
| `weirctl hotkey add NAME [options]` | Add one. |
| `weirctl hotkey change HOTKEY [options]` | Change one. Steps given replace all its steps; `--name` renames it, `--add-keys KEYS` gives it other keys as well, and `--no-keys` takes its keys away. |
| `weirctl hotkey remove HOTKEY` | Remove one. |
| `weirctl hotkey run HOTKEY` | Do what tapping its keys does. |
| `weirctl hotkey press HOTKEY` | Do what pressing its keys does, until `release`. |
| `weirctl hotkey release HOTKEY` | Do what letting go of its keys does. |
| `weirctl hotkey on\|off\|toggle HOTKEY` | Switch its keys on or off. Off, it can still be run by name. |
| `weirctl hotkey move HOTKEY [--group GROUP] [--to N]` | Move it into a group (`none` for no group), and to place N among the group's hotkeys, counting from 0; last when `--to` is left out. |
| `weirctl hotkey group add NAME [--off]` | Add a group of hotkeys, last in the list. |
| `weirctl hotkey group on\|off\|toggle GROUP` | Switch a group's hotkeys' keys on or off. Each hotkey keeps its own switch. |
| `weirctl hotkey group rename GROUP NAME` | Rename a group. |
| `weirctl hotkey group move GROUP --to N` | Move a group to place N among the groups. |
| `weirctl hotkey group remove GROUP` | Remove a group. Its hotkeys stay, in no group. |
| `weirctl hotkey settings` | Open the desktop's shortcut settings at Weir's hotkeys (KDE Plasma 6.5 and newer). |

HOTKEY is a hotkey's name or id, and GROUP a group's. `add` and `change`
take:

| Option | |
|---|---|
| `--keys KEYS` | Such as `Ctrl+Alt+M`: any of Ctrl, Alt, Shift and Super, then one key. Letters, numbers and the like need Ctrl, Alt or Super; F1 to F24, media keys and Pause can be on their own. [The full list](API.md#keys). Give it more than once for several, any of which presses the hotkey; on desktops other than KDE Plasma that look after the keys, only the first is suggested. It replaces the keys the hotkey had. |
| `--do STEP` | A step: a method and its parameters as JSON, `'set_strip {"id": "Mic", "mute": "toggle"}'`, or a whole [step](API.md#hotkeystep) as JSON, for a fade. Once per step, in order. |
| `--each-press all\|next` | Every step at each press (the default), or the next one, going round. |
| `--release nothing\|restore\|steps` | What letting go does: nothing, put back what pressing changed, or the `--release-do` steps. |
| `--release-do STEP` | A step for letting go, like `--do`. |
| `--repeat MS` | Do the steps again every MS milliseconds while held, 20 to 2000; `0` not to. |
| `--enabled on\|off\|toggle` | Switch its keys on or off. Off, it can still be run by name. |
| `--group GROUP` | The group it is in, or `none`. |

```sh
weirctl hotkey add "Mic on/off" --keys Ctrl+Alt+M --do 'set_strip {"id": "Mic", "mute": "toggle"}'
weirctl hotkey add "Talk" --keys F9 --keys Ctrl+Alt+T --do 'set_strip {"id": "Mic", "mute": false}' --release restore
weirctl hotkey add "Music down" --keys Ctrl+Alt+Down --do 'set_strip {"id": "Music", "gain_delta_db": -2}' --repeat 150
weirctl hotkey add "Fade out" --do '{"method": "set_bus", "params": {"id": "A1", "gain_db": -60}, "over_ms": 3000}'
weirctl hotkey add "Scenes" --keys Ctrl+Alt+S --each-press next --do 'load_scene {"name": "Streaming"}' --do 'load_scene {"name": "Late night"}'
weirctl hotkey change "Music down" --keys Ctrl+Shift+Down
weirctl hotkey change "Music down" --add-keys VolumeDown
weirctl hotkeys
weirctl hotkey run "Mic on/off"
weirctl hotkey off "Mic on/off"
weirctl hotkey group add Games
weirctl hotkey move "Music down" --group Games
weirctl hotkey move "Talk" --to 0
weirctl hotkey group off Games
weirctl hotkey group rename Games Gaming
weirctl hotkey remove "Fade out"
```

`weirctl hotkeys` starts with how keys reach Weir. On KDE Plasma and other
Wayland desktops with a shortcut service, the desktop looks after the keys:
each hotkey is one entry in its shortcut settings. On Plasma, Weir gives
the entry all of a hotkey's keys, and keys changed there come back; on
other desktops only the first is suggested, and more are added there.
`weirctl hotkeys` shows every key the desktop has for each hotkey, and
`weirctl hotkey settings` opens those settings.

## Settings and the window

### `weirctl settings [options]`

Without options, shows the daemon's settings. With them, changes them.

| Option | |
|---|---|
| `--solo exclusive\|BUS` | What soloing does: silence the other strips in every mix, or only change one bus's mix, such as your headphones. |
| `--rate HZ\|auto` | Hold PipeWire at this sample rate while Weir runs. 48000 is recommended: noise suppression only works there. |
| `--buffer FRAMES\|auto` | Hold PipeWire at this buffer size while Weir runs. Smaller means less delay and more CPU. |
| `--meter-rate N` | Meter updates a second. |
| `--startup window\|minimized\|tray-only` | What happens when Weir starts. |
| `--start-at-login on\|off\|toggle` | Start Weir when you log in, from the next login. Needs Weir installed as the install steps describe. |
| `--tray on\|off` | Show the tray icon, from the next start. |
| `--tray-icon color\|one-color` | The tray icon in color, or in one color like the panel's other icons. |

```sh
weirctl settings
weirctl settings --rate 48000 --buffer 256
weirctl settings --solo A1
weirctl settings --rate auto --buffer auto --solo exclusive
```

<!-- not tested: changes how the computer running the check starts up -->
```sh
weirctl settings --start-at-login on
```

### `weirctl show`

Brings the mixer window to the front, or opens it.

<!-- not tested: needs a display -->
```sh
weirctl show
```

## Watching and anything else

`weirctl watch` prints every change as it happens, from anywhere, until
you press Ctrl+C. With `--meters` it prints the meters too.

<!-- runs until stopped -->
```sh
weirctl watch --meters
```

`weirctl raw METHOD [PARAMS]` sends any request of the
[control protocol](API.md), with its parameters as JSON, and prints the
answer. Everything the protocol can do is available this way, including
what has no command of its own:

```sh
weirctl raw set_strip '{"id": "Music", "sends": {"B1": -6}}'
weirctl raw set_bus '{"id": "B1", "limiter": {"release_ms": 150}}'
weirctl raw describe
```

## A mute key for any desktop

Where `weirctl hotkeys` says hotkeys only work by name, the desktop's own
custom shortcuts can run them, or any other command. In your desktop's
keyboard settings, add a custom shortcut with a command such as

<!-- not tested: an example of a command to bind -->
```sh
weirctl hotkey run "Mic on/off"
```

or, without a hotkey,

<!-- not tested: an example of a command to bind -->
```sh
weirctl strip Mic --mute toggle
```

and give it a key. For push to talk, bind `weirctl hotkey press Talk` to
pressing a key and `weirctl hotkey release Talk` to letting it go, if your
shortcut tool can tell the two apart.
