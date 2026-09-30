# Contributing to Weir

Thank you for helping. Bug reports, testing on real hardware and code are
all welcome.

**Reporting a bug.** Say what you did, what you expected and what happened
instead. If the daemon stopped, the window's **Copy the message** button
copies what it said; the whole log is in `~/.local/state/weir/daemon.log`,
or `journalctl --user -u weir` if it runs as a service. Weir has mostly
been developed against a headless PipeWire, so reports from real sound
cards, USB headsets and unusual setups are especially useful.

[How Weir works](docs/ARCHITECTURE.md) is the map of the code; read it
before a larger change.

## Building

You need Rust 1.88 or newer, a C compiler, `make`, `pkg-config`, and
clang with PipeWire's development files, from which the bindings are
generated at build time. The [install steps](README.md#install) put all
of them in place, for each system Weir has been tested on, including how
to get a new enough Rust where the system's own is older.

```sh
cargo build --workspace          # debug builds of everything
make                             # release builds, as installed
```

## Before sending a change

All four must pass:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets     # no warnings
cargo test --workspace                     # needs no PipeWire
docs/check_examples.py --socket ...        # see below, when the protocol or CLI changed
```

GitHub runs the first three on every pull request, together with the
checks below, and shows the result on the pull request.

After adding or updating a dependency, run `make licenses` (it needs
`cargo install cargo-about --features cli`) and commit
`packaging/licenses/THIRD-PARTY-LICENSES.txt`. Packages carry that file,
because the libraries' licenses ask for their notices to go with every
copy of Weir, and the checks fail when it is out of date.

Weir builds on Rust 1.88 (`rust-version` in `Cargo.toml`), the oldest its
window library, egui, supports, so standard library methods newer than
that are not available. Updating a dependency can raise the minimum; check
that a change still builds with it:

```sh
rustup toolchain install 1.88
cargo +1.88 build --workspace --locked
```

## Running it from the checkout

Give the daemon its own configuration and socket, so it does not touch
your installed Weir:

```sh
cargo build --release
./target/release/weir-daemon --config /tmp/weir-dev/config.toml --socket /tmp/weir-dev/c.sock --no-window
./target/release/weir --socket /tmp/weir-dev/c.sock --no-spawn
./target/release/weirctl --socket /tmp/weir-dev/c.sock state
```

`RUST_LOG=debug` makes the daemon say more. `qpwgraph` or `pw-link -l`
shows the engine node, `Weir`, with a port per strip and bus channel, the
virtual devices (`weir.input.N`, `weir.output.N`) and the links the daemon
keeps.

## Trying it without a sound card

A headless PipeWire runs everything but real devices, which is how most of
Weir has been tested:

```sh
export XDG_RUNTIME_DIR=/tmp/weir-xdg; mkdir -p -m 700 $XDG_RUNTIME_DIR
pipewire &
dbus-daemon --session --fork --print-address > $XDG_RUNTIME_DIR/bus
DBUS_SESSION_BUS_ADDRESS=$(cat $XDG_RUNTIME_DIR/bus) wireplumber &
Xvfb :99 -screen 0 1600x1000x24 & export DISPLAY=:99     # for the window

./target/release/weir-daemon --config /tmp/weir/config.toml --socket /tmp/weir/c.sock --no-window --no-tray &
pw-play --target weir.input.2 some.wav &                  # plays into strip 2
./target/release/weirctl --socket /tmp/weir/c.sock watch --meters
LIBGL_ALWAYS_SOFTWARE=1 ./target/release/weir --socket /tmp/weir/c.sock --no-spawn &
```

Things that trip this up:

* Socket paths longer than about 100 bytes fail to bind; keep them short.
* WirePlumber restores a stream's last volume, so a test tone can arrive
  quieter than expected: `weirctl app-volume <id> --gain 0`.
* With no window manager, new windows open at the top left over the mixer,
  and `xdotool windowfocus --sync` is needed before typing.
* `xdotool windowclose` destroys the window under winit, which crashes it;
  that is the test, not Weir.

## Checking the documentation's examples

`docs/check_examples.py` runs every example in [the API reference](docs/API.md)
and [the command line reference](docs/CLI.md) against a running daemon,
resetting the mixer before each. Run it after changing the protocol or
`weirctl`. It needs a daemon with a fresh configuration, and an application
called Firefox playing, for the application examples:

```sh
./target/release/weir-daemon --config /tmp/weir-docs/config.toml --socket /tmp/weir-docs/c.sock --no-window --no-tray &
cat /dev/zero | pw-cat --playback --target weir.input.3 -P '{ application.name = "Firefox" }' - &
docs/check_examples.py --socket /tmp/weir-docs/c.sock
```

## Rules that keep Weir working

* **The real-time thread** (`engine/src/dsp/process.rs` and everything it
  calls) never allocates, locks, makes system calls or logs. Parameters
  reach it as an immutable `RtParams` snapshot through `ArcSwap`; state
  that must survive a new snapshot (filter memories, envelopes) lives in
  `FxState`, shared between snapshots by `Arc` and touched only by that
  thread. Buffers are allocated when the snapshot is built. Every change of
  level ramps, and effects switched on or off crossfade, so nothing clicks.
* **The daemon is the source of truth.** Clients send requests and mirror
  notifications. Every change to the mixer goes through
  `Controller::mutate`, which normalizes the state, records undo, hands it
  to the engine and tells every client. Give a new request that changes the
  mixer an undo label in `controller/undo_labels.rs`.
* **Protocol types are the schema.** New fields get `#[serde(default)]`
  (and usually `skip_serializing_if`), so old configuration files and old
  clients keep working. Changes are patches of `Option`s; on/off fields use
  `Flag`, which takes `"toggle"`; values dials change get a `*_delta_*`
  field. `describe` generates JSON Schemas from these types, so their doc
  comments are documentation too.
* **A change to what the configuration means** raises `CONFIG_VERSION` in
  `daemon/src/config.rs`, with a migration step and a test.
* **Weir never sets the system's volume** on its own devices. It shows it;
  only application streams' volumes are set, and only when asked.
* **PipeWire reuses ids**, of nodes and of applications. Tell nodes apart
  by `object.serial`, and applications by id and name together.
* **Renaming a strip, or changing its layout, recreates its device.** The
  runner moves its applications back afterwards (`pending_moves`).

## Adding a feature, end to end

1. The model and its patch in `protocol`, with `normalize` bringing values
   into range and removing references to strips or buses that are gone.
2. The engine: the real-time part, with tests through the `Rig` in
   `dsp/process.rs`, and a meter if it has something worth showing.
3. The daemon: handling the request, its undo label, and storing it, with
   a configuration migration if needed.
4. `weirctl`: its options and what it prints.
5. The window.
6. The documentation: `docs/API.md` (with examples, checked by
   `docs/check_examples.py`), `docs/CLI.md`, `docs/USER_GUIDE.md`, and the
   README if it changes what Weir is.

## Style

* **American spelling** everywhere: color, equalizer, analyze, gray.
* **Comments say why**, in plain sentences; the code says what. Match the
  amount of commenting in the file you are in.
* **Text people read** in the window, the command line or the docs is
  short and friendly, explains any jargon, and says what a control does to
  the sound. The people Weir is for are often new to Linux audio.
* **Colors** come from `theme::p()`, the palette in use, which follows the
  desktop's light or dark setting. A new color goes into both `DARK` and
  `LIGHT` in `theme.rs`, never as a literal where it is drawn; check it on
  both.
* **egui's fonts lack many symbols**: `▸ ▾ ↓ → ✓ ✕` show as boxes, while
  `× · • … › ± − ⚙` work. Draw anything else with the painter, or use words.
* **Icons** (`packaging/weir.svg` and `weir-symbolic.svg`) stay plain shapes
  and gradients, with no masks, clip paths or filters, which KDE may not
  draw. The one-color icon is filled shapes with the `ColorScheme-Text`
  class, so Plasma and GNOME can recolor it. After changing the color icon,
  `make icons` redraws the PNG built into the window.
* **Commit messages** have a short subject in the imperative ("Add a
  limiter to buses"), then a body saying what changed and why, wrapped at
  72 columns.

## Installing and packaging

`make install` installs into `~/.local`, and `make uninstall` removes
exactly that. `make rpm` builds an RPM from the last commit into
`~/rpmbuild/RPMS`, and `make rpm-install` installs it with dnf. `make deb`
builds a `.deb` from the working tree into `target/deb` (it needs
`dpkg-dev`). Both packages hold what `make install` puts down; the RPM spec
in `packaging/weir.spec` also lists those files, so change it with the
Makefile. `make rpm-install` numbers its package by date and time, so dnf
counts it as newer than a released package of the same version: run
`sudo dnf remove weir` before installing a downloaded one.

GitHub builds both packages whenever a pull request changes the
packaging, and on demand from **Actions → Packages → Run workflow**. Each
run keeps them as downloads on its page for two weeks, for trying before a
release.

## Making a release

1. Choose the new version number, such as `1.1.0`, and put it in three
   places: `version` in `Cargo.toml`, `Version:` and a new `%changelog`
   entry in `packaging/weir.spec`, and a new `<release>` in
   `packaging/io.github.mourty.weir.metainfo.xml`, which software centers
   show as the release notes.
2. Merge that to `main` and wait for the checks to pass.
3. On GitHub, open **Releases → Draft a new release**. Under **Choose a
   tag**, type `v1.1.0` (the version with a `v` in front) and pick
   **Create new tag on publish**. Give it a title, write what changed (or
   press **Generate release notes**), and press **Publish release**.
4. The **Packages** workflow builds the RPM and the `.deb` and attaches
   them to the release, with a `SHA256SUMS` file, in about twenty minutes.
   It refuses to start if the tag and the version numbers disagree.
   It also signs an attestation for each package (listed on the
   **Actions** tab under **Attestations**). Only files the workflow built
   carry one, so never attach packages to a release by hand: if a release
   run fails, fix the workflow, delete the release and its tag, and
   publish again.

By contributing you agree that your work is released under the
[MIT license](LICENSE), like the rest of Weir.
