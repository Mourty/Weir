# How Weir works

This is for people who want to change Weir, or just see how it is put
together. It explains the parts, how sound gets through them, and why they
are built the way they are. The code's own comments go into the detail;
this is the map.

## The parts

```
          weir (window)     weirctl     your scripts, a Stream Deck plugin ...
                 \             |              /
                  JSON-RPC over a Unix socket, one message per line
                                |
                          weir-daemon
          state, config, undo history, app rules, tray icon
                                |
                          weir-engine
       the PipeWire thread, and the real-time mixer in one pw_filter
                                |
                             PipeWire
```

Weir is a **daemon** that does the work and **thin clients** that talk to
it. The window has no special access: everything it does goes through the
same [control protocol](API.md) as `weirctl` or anyone's script, so that
protocol is complete by construction and exercised every time someone moves
a fader. It also means the audio devices stay put while the window is
closed, and the tray icon can live in the process that keeps running.

The code is a Cargo workspace of five crates:

| Crate | Builds | What it is |
|---|---|---|
| `crates/protocol` | a library | The data model, every request and notification, and the maths clients must agree with the engine on (equalizer curves, the compressor's curve, scenes). No PipeWire, no UI. |
| `crates/engine` | a library | The PipeWire side and the real-time mixer. `dsp` is pure Rust and testable without PipeWire; `pw` owns the PipeWire connection. |
| `crates/daemon` | `weir-daemon` | The state, the configuration files, undo, application rules, scenes and setups, the socket server and the tray icon. |
| `crates/cli` | `weirctl` | The command line client. |
| `crates/gui` | `weir` | The window, in egui. |

## Sound through PipeWire

Every bit of mixing happens in **one PipeWire node**, a `pw_filter` called
"Weir", with an input port for each strip channel and an output port for
each bus channel. PipeWire calls it once per cycle, on its real-time
thread, with a buffer for every port; the filter mixes them all in that one
callback. Nothing is added to PipeWire's own latency.

Around it, the daemon creates **virtual devices** and **links**:

```
 Firefox ──▶ [weir.input.3 "Browser (Weir)"] ──monitor──▶ ┌─────────────┐ ──▶ [headset]              A1
 Spotify ──▶ [weir.input.2 "Music (Weir)"]   ──monitor──▶ │ Weir engine │
 [microphone] ──────────────────────────────────────────▶ │  pw_filter  │ ──▶ [weir.output.3 "Stream Mic (Weir)"] ──▶ Discord   B1
                                                          └─────────────┘
```

* A **virtual strip** is a `support.null-audio-sink` adapter with
  `media.class = Audio/Sink`: a playback device applications can choose.
  Its monitor ports feed the engine's inputs for that strip.
* A **virtual bus** is the same adapter as `Audio/Source/Virtual`: a
  microphone applications can record from, fed by the engine's outputs.
* A **hardware** strip or bus links the engine's ports straight to the
  device's.
* A strip or bus with **external effects** on gets two more adapters:
  "*name* to effects (Weir)" (`weir.to-effects.strip.N`, like a virtual
  bus), fed from the engine's `to_effects_*` ports, which an effects
  program records from, and "*name* from effects (Weir)"
  (`weir.from-effects.strip.N`, like a virtual strip), which it plays
  into. The monitor of "from effects" does not go back into the engine but
  into a second node, "Weir effects return" (`weir.effects-return`, also a
  `pw_filter`), with a port per channel of each, which exists while any
  external effects are on. Each cycle, the return node leaves what came
  back in a lock-free hand-off (`dsp/handoff.rs`), and the engine takes it
  on its next cycle. So there is no loop for PipeWire to see: the engine
  coming back into itself would be one, which PipeWire runs only when
  every step of it is a link, and many effects programs, filter chains and
  `pw-loopback` among them, record and play through two nodes with no link
  between them, which froze the engine. In a patchbay, it reads as a line,
  out of Weir, through the effects and back into Weir, with no wire
  doubling back (`runner/effects.rs`).
* Device descriptions leave out colons ("Music - Chat (Weir)" for a strip
  called "Music: Chat"): JACK programs such as Carla, which see PipeWire
  through its JACK library, take everything before a colon for the
  program's name, and would draw all devices of strips whose names begin
  the same way as one block.
* The engine and the return node share a `node.group`, so one clock always
  drives both, and Weir's devices join them through their links. Both
  have `node.autoconnect = false`, so the session manager leaves their
  wiring to Weir.

The PipeWire side runs on a thread of its own (`pw/runner`). It keeps a
mirror of PipeWire's registry, and whenever anything changes, on either
side, it **reconciles**: it brings the engine's ports, the real-time
parameters, the virtual devices and the links in line with the mixer state
the daemon wants, in that order. So a device unplugged and plugged back in
is simply linked again, a link of Weir's removed in a patchbay is made
again, and a strip added in the window gets its device, ports and links
from the same code that set everything up at the start.

Two things about PipeWire shape that code. It **reuses ids**, both of nodes
and of application streams, so nodes are told apart by `object.serial` and
applications by id and name together. And a strip's device has to be
**recreated** when the strip is renamed or its layout changes; the runner
remembers which applications were playing into it (`pending_moves`) and
moves them back once the new device is there. External effects' devices
are remade the same way, and the links other programs had with them are
noted and made again (`pending_relinks`), as lingering links that belong
to PipeWire, like the ones they replace.

Whether anything plays into a "from effects" device, or straight into the
return node, is read from the links in the registry mirror. It decides, in the snapshot, whether the
strip or bus carries on with what comes back or with its fallback, and the
daemon passes it on so the window can warn about effects that are not
connected.

## The real-time mixer

The rules on PipeWire's real-time thread are strict: **no allocation, no
locks, no system calls, no logging**, because any of them can stall the
audio. The whole design of `engine/src/dsp` follows from that.

**Parameters arrive as snapshots.** The daemon's thread turns the mixer
state into an immutable `RtParams`: every level as a linear gain, every
route as a matrix of coefficients from the strip's channels to the bus's
speakers, every effect's settings in the form the real-time code wants.
Every buffer the real-time code will need is allocated here. The snapshot
is published through an `ArcSwap`, and the real-time thread picks up the
newest one at the start of each cycle, with one atomic load.

**Changes never click.** Every level, pan, mute and route ramps linearly
over 10 ms, and a new snapshot takes over the ramps where the old one left
them. Effects switched on or off crossfade, and an equalizer whose bands
change crossfades from the old filters to the new ones.

**Effect state outlives snapshots.** Filter memories, envelopes and the
noise suppressor's frames must carry on across a change of settings, and
some of them are too big to copy. They live in an `FxState` per strip and
bus, which each new snapshot shares by `Arc` with the one before, and which
only the real-time thread touches.

**One cycle**, for each block of samples:

1. Every bus output, and every output to external effects, is cleared.
2. Each strip runs its chain: **noise suppression**, **gate**,
   **equalizer**, **compressor**, then its **fader**, mute and pan. Its
   meter reads here. It is then added into each bus it is routed to,
   through that bus's matrix, at its level in that bus's mix, ducked in the
   mixes its ducking covers, with its subwoofer and upmix feeds.
3. Each bus folds to **mono** if asked, runs its **equalizer**, applies its
   **fader**, then its **limiter**, and meters what comes out.

**External effects** can sit between any two of those stages of a strip
or bus, or after a strip's fader: there the sound is copied to the
outputs to "to effects", and replaced by what came back through "from
effects", taken from the return node's hand-off. After a strip's fader, the fader is applied before
sending, and what comes back is mixed at unity, ducking and send levels
still applying.

**Meters** are atomics the real-time thread raises to each new peak; the
daemon reads and resets them 30 times a second. The **spectrum** behind
the equalizer curve works the same way in spirit: while a window watches a
strip, the real-time thread copies its equalizer's input and output, mixed
to mono, into a ring of atomics, and the daemon runs an 8192-point FFT over
them 30 times a second and folds the result into 192 points for the window.
Nothing is copied when nobody watches.

### Channels

Every strip and bus has its own layout, from mono to 7.1 or any list of
speaker positions, and `dsp/mapping.rs` decides how each strip channel
lands on each bus speaker: the same position goes to the same speaker; a
mono strip goes to both fronts; what a bus has no speaker for is
**downmixed** (the ITU-R BS.775 standard, a Pro Logic II matrix, or one
part only); and speakers a strip has no channel for can be filled by
**upmixing** (center fill, all-channel stereo, passive surround).

### The effects

| Effect | How it works |
|---|---|
| Equalizer | Up to 16 bands, each a trapezoidal state variable filter, the same shapes as the Audio EQ Cookbook's biquads but well behaved when changed quickly. The maths is in the protocol crate, so the window draws exactly the curve the engine applies. |
| Noise suppression | RNNoise, through the pure Rust `nnnoiseless`, so there is no C library to package. It works on 10 ms frames at 48 kHz, which is why it needs that rate, and delays its strip by 20 ms. |
| Gate | A peak detector with attack, hold and release, which closes 4 dB below its threshold so a level hovering around it does not make it flutter. |
| Compressor | A feed-forward compressor with a 6 dB soft knee, and a lift that can be worked out from the threshold and ratio. Its curve is in the protocol crate, for the window's graph. |
| Ducking | Each strip's level after its fader, gated by its gate, is compared with a threshold; the strips it ducks are turned down in the mixes their ducking names. |
| Limiter | Looks 1.5 ms ahead, with the channels of a bus linked so the image does not shift. |
| External effects | Not an effect itself: the sound goes out and comes back, crossfading over 10 ms between what comes back, the sound as it went out, and silence, as programs connect and go. |

`dsp/process.rs` has a test `Rig` that drives the processor without
PipeWire, and the tests read like the behavior they check: a gate closes
on noise and opens for speech, switching the equalizer on does not click,
the limiter holds its ceiling without touching quieter audio.

## The daemon

**One source of truth.** `Controller` holds the mixer state, what the
engine reports (devices, applications, the engine's status, system
volumes), the settings and the undo history, behind one lock. Clients only
ever send requests and mirror what they are told.

**Every request** arrives on the socket (`server.rs`, one tokio task per
connection), has the names in it resolved to ids (`controller/names.rs`),
and is handled in `controller/handlers.rs`. **Every change to the mixer**
goes through `Controller::mutate`, which:

1. applies the change to a copy of the state, and **normalizes** it:
   values into their ranges, references to strips and buses that are gone
   removed;
2. records the state from before for **undo** (a run of changes to the same
   fields of the same thing within 1.5 s is one step; 100 steps are kept);
3. hands the new state to the **engine**;
4. marks the configuration to be **saved** (at most every half second,
   written to a new file and renamed over the old one, so a crash cannot
   leave half a file);
5. and **notifies** every subscribed client.

**Configuration** lives in `~/.config/weir` as TOML. The file carries a
version; when a change would alter what an old file means, the version goes
up and `config.rs` gains a migration step, with a test. At startup the
daemon keeps a copy of the configuration in `backups/`, the last ten.

**Application rules** are applied as applications appear: the first
matching rule moves an application once, retrying a few times if the
session manager moves it back, and then leaves it alone, so moving it by
hand is respected.

**The tray icon** is in the daemon, because the daemon is what keeps
running when the window is closed. It talks to the rest through a channel,
since its callbacks run on their own task.

**Hotkeys** (`hotkeys/`) are steps, each an ordinary request, so a hotkey
can do whatever the protocol can and needs nothing of its own in the
engine. The controller keeps the list (`controller/hotkeys.rs`: checking,
saving to `hotkeys.json`, noticing strips that are gone); a **runner**
(`hotkeys/runner.rs`) does the work. It is one task with a mailbox that
key presses, its own repeat and fade timers and a change of hotkeys all
arrive in; a client pressing a hotkey calls it directly, under its lock,
so the answer comes once the steps are done. Steps run through the same
handler as requests, but without their own undo step: the runner records
each press as one, from the mixer before the keys went down to the mixer
once they are up and its fades are over. Putting back on release
(`hotkeys/restore.rs`) compares the mixer before the press with the mixer
right after the hotkey's own steps, as JSON with strips and buses matched
by id, and sets back only those places. Undo steps recorded while the key
was held are rewritten the same way, so undoing a fader moved while
talking does not open the microphone again.

The **keys** (`hotkeys/keys/`) come one of two ways. On Wayland no program
may watch the keyboard, so Weir asks the desktop through the XDG desktop
portal's global shortcuts (`portal.rs`, with `ashpd`): it suggests keys,
the desktop decides and says when they go down and up, and people can
change them in the desktop's settings. Each hotkey is one shortcut, which
takes one suggestion, so only its first keys are suggested; people add
more to the same entry in the desktop's settings, and the window shows
what the desktop reports. A portal session can bind only once, so a change
of hotkeys closes it and opens a new one, binding the whole list even when
empty: the desktop forgets shortcuts left out, which is how a removed
hotkey leaves its settings. A new session starts with the shortcuts the
desktop already has, and a switched-off hotkey stays bound if it is one of
them, so its keys are not lost, but is not bound for the first time while
off, so examples added switched off do not make the desktop ask about
keys. Shortcut ids carry a hash of the suggested keys, so changed keys are
offered afresh. The portal files shortcuts under the program's
name and refuses them without one, but works the name out only for
programs started from the application menu, not for the daemon started at
login. So the daemon first tells the portal it is `weir` (its `Register`
call), which the portal takes only because `weir.desktop` is installed. A
portal that restarts forgets both the name and the shortcuts, so Weir
starts over when it does. On X11 Weir grabs the keys on the root window
itself (`x11.rs`, with `x11rb`), with and without Caps Lock and Num Lock,
and asks XKB not to repeat held keys as presses. Anywhere else hotkeys are
pressed only by name, which the desktop's own shortcuts can do with
`weirctl hotkey run`. At login the daemon waits for the desktop first, as
it does for the window. `WEIR_HOTKEYS=desktop`, `x11` or `none` picks the
way, for testing.

**Starting at login** is systemd's to keep, not the configuration's:
Weir starts at login when its user unit, `weir.service`, is enabled. The
daemon asks `systemctl --user is-enabled` when it starts and after
changing it with `enable` or `disable` (in `login.rs`), so the setting
cannot drift from what systemd will actually do. It never uses `--now`:
the daemon asking is already running.

## The protocol

The protocol crate's types *are* the protocol. `describe` returns JSON
Schemas generated from them, with their doc comments as descriptions, so
the schemas and the Rust types cannot disagree. A few conventions keep it
friendly and compatible:

* New fields get `#[serde(default)]`, and usually
  `skip_serializing_if`, so old configuration files and old clients keep
  working, and messages stay small.
* Changes are patches: every field an `Option`, absent meaning unchanged.
  Fields that can be cleared use a double option, so `null` and absent
  differ.
* On/off fields in patches are a `Flag`, which also takes `"toggle"`, and
  values that dials change have a `*_delta_*` twin.
* Strips and buses can be named instead of numbered anywhere, resolved by
  the daemon before the request is parsed.
* Replies are built with `to_json`, which keeps 32-bit floats as short as
  they are (`6.7`, not `6.699999809265137`).

## The window

The window (`crates/gui`) is an egui application and a client like any
other. A connection thread sends its requests and a reader thread mirrors
notifications into shared state; each frame, the window copies what it
needs and draws.

Two things keep it feeling immediate. A control being dragged keeps its own
value for a moment after the last change, so an echo of an older value
from the daemon cannot pull it back under the pointer, and it sends at most
every 40 ms. And meters are animated in the window between the daemon's
readings, falling smoothly and holding their peaks.

Each strip's and bus's settings window is a separate native window (an
egui viewport), so it can sit on another screen. Colors come from two
palettes, dark and light, and the window follows the desktop's preference
through the XDG desktop portal.

The Hotkeys window and the hotkey editor (`hotkeys/`) are viewports too.
Most hotkeys are made in a simple form (`hotkeys/simple.rs`): one action
on one strip or bus, turned into steps, and read back from them, so a
hotkey opens in the form it was made in; any other opens with all its
options, as a list of steps and as JSON. Requests bring the window no
replies, so after Save the editor stays open until the daemon's hotkeys
change, or shows the error the daemon sent after it saved.

## Testing

* `cargo test --workspace` runs over 150 unit tests without PipeWire:
  the real-time processor through its test rig, every effect,
  the protocol's parsing and maths, the daemon's request handling, undo,
  configuration migrations, and the window's logic.
* A headless PipeWire runs everything but real devices; `CONTRIBUTING.md`
  says how. The daemon, the window and `weirctl` all work against it.
* `docs/check_examples.py` runs every example in the API and command line
  references against a running daemon, so the documentation cannot drift
  from the code.

## Why it is built this way

**Why not an existing tool?** pulsemeeter drives PulseAudio modules from
Python with no real-time engine of its own; jack_mixer has strips and
meters but JACK ports only, which applications cannot choose as an output;
qpwgraph, Helvum and Sonusmix route but do not mix; PipeWire's own
`module-loopback` and `filter-chain` are building blocks configured in
files, with no live control. PipeWire's client API offers everything
needed, so Weir uses it directly.

**Why one engine node?** The alternative was a PipeWire stream per strip
and bus, letting the session manager do the linking. That makes choosing
devices simpler, but mixing across many nodes within one cycle depends on
how PipeWire schedules them, and the safe way around that is a ring buffer
that adds a cycle of latency, as `module-loopback` does. One node mixing
everything in one callback is sample-aligned and adds nothing, and its
correctness is easy to argue.

**Why Rust and egui?** Real-time audio without a garbage collector, safe
concurrency between the audio and control threads, and one language for
all of it. The `pipewire` crate has no safe `pw_filter` wrapper, so the
engine carries a small unsafe one (`pw/filter.rs`). egui makes custom
controls such as faders and meters easy to draw, and the window a pure
function of the state.

**Why external effects rather than plugins?** Hosting LV2, CLAP or VST
plugins means loading other people's code into the real-time thread,
plugin windows, presets and crash isolation: a project of its own, which
Carla already is. A send and a return per strip or bus lets Carla,
EasyEffects or anything else that PipeWire can link do the effects, for
the price of one cycle of latency, and keeps Weir a mixer.

**Why hotkeys in the daemon?** The daemon is what keeps running when the
window is closed, and a hotkey that only worked while the window was open
would be a surprise. Steps as requests, rather than a list of actions of
their own, mean anything new in the protocol can be a hotkey's step at
once, and that a script, a Stream Deck button and a key can share one.

**Why only a local socket?** It needs no password: only the user running
Weir can open it. A TCP listener could be added in the server alone, if
remote control is ever wanted.

## What is next

* Testing on more real hardware; most of the newer features have been
  developed against a headless PipeWire.
* An OpenDeck plugin, as its own project on top of the protocol.
* Perhaps: a routing matrix view for many strips, and positioning sources
  in a sound field.
