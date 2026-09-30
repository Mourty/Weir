<p align="center"><img src="packaging/weir.svg" width="112" alt=""></p>

<h1 align="center">Weir</h1>

<p align="center">A Voicemeeter-style audio mixer for Linux, built on PipeWire.</p>

![Weir's mixer window](docs/images/mixer.png)

Weir puts the sound on your computer on a mixing desk. Your microphone,
your music, your browser and your game each get a **strip** with a fader.
Your headphones, your speakers and a virtual microphone for Discord or OBS
each get a **bus**. A row of buttons beside every fader decides who hears
what.

So you hear the game and your friends. Your stream hears your voice and the
music, a little quieter, and never the hum of your fan. And every level is
one drag away, in one window.

*A weir is a low dam across a river: it sets how high the water stands and
lets the rest flow on.*

## What it does

* **As many strips and buses as you need.** Each virtual strip is a playback
  device of its own that applications can choose, such as "Music (Weir)",
  and each virtual bus is a microphone other programs can record from.
* **A separate mix for every output.** A strip can be louder in your
  headphones than on your stream, and the faders can switch to show any
  bus's mix.
* **Effects for voices**: noise suppression, a noise gate, a compressor,
  and a 16-band equalizer with a live analyzer behind its curve. **Ducking**
  turns the music down while you talk, and a **safety limiter** keeps every
  output from distorting.
* **Surround**, up to 7.1, with the standard ways of fitting surround onto
  stereo and stereo onto surround.
* **Scenes and setups** save how things sound and how the mixer is laid
  out, **rules** put applications on the right strip as they start, and
  every change can be **undone**.
* **Scriptable.** `weirctl` does everything the window does from a
  terminal or a hotkey, and a documented protocol lets any program, such
  as a Stream Deck plugin, do the same.
* **Made for the desktop it runs on.** All the mixing happens in a single
  PipeWire node, adding nothing to PipeWire's own latency. Weir follows
  your light or dark theme, lives in the tray, and keeps running when its
  window is closed.

![A strip's settings: noise gate, compressor and equalizer](docs/images/settings.png)

## Install

### Download a package

The easiest way. On the [latest release](https://github.com/Mourty/Weir/releases/latest),
under **Assets**, download the file for your system and open it: your
software center installs it. Or install it from a terminal, in the folder
you saved it to:

| System | File | In a terminal |
|---|---|---|
| Fedora, Nobara | the one ending in `.rpm` | `sudo dnf install ./weir-*.rpm` |
| Ubuntu 24.04 and newer, Linux Mint 22, Pop!_OS 24.04, Debian 13 | the one ending in `.deb` | `sudo apt install ./weir_*.deb` |

A downloaded package does not update itself. To hear about new releases,
choose **Watch → Custom → Releases** at the top of Weir's GitHub page.
To remove Weir, run `sudo dnf remove weir` or `sudo apt remove weir`.

If you built Weir yourself before, remove that copy first:
`sudo dnf remove weir` on Fedora and Nobara, where your own build would
otherwise count as newer, or `make uninstall` in its `Weir` folder
elsewhere. Your settings are kept either way.

### Or build it yourself

For Arch and CachyOS, other systems, or the newest changes. It takes five
to fifteen minutes, and has been tested on each of the systems below.
Open a terminal and follow the steps for yours:

<details>
<summary><b>Fedora and Nobara</b></summary>

```sh
sudo dnf install git-core cargo rust clang make pipewire-devel pkgconf-pkg-config rpm-build rpmdevtools desktop-file-utils libappstream-glib
git clone https://github.com/Mourty/Weir
cd Weir
make rpm-install
```

This installs Weir as a package, which `dnf` updates and removes like any
other.

</details>

<details>
<summary><b>Ubuntu 25.10 and newer</b></summary>

```sh
sudo apt update
sudo apt install git build-essential clang pkg-config libpipewire-0.3-dev cargo rustc
git clone https://github.com/Mourty/Weir
cd Weir
make install
```

</details>

<details>
<summary><b>Linux Mint 22, Pop!_OS 24.04 and Ubuntu 24.04</b></summary>

These come with Rust 1.75, which is too old to build Weir, but offer a
newer one alongside it. The `PATH=…` at the start of the last line tells
the build to use it:

```sh
sudo apt update
sudo apt install git build-essential clang pkg-config libpipewire-0.3-dev cargo-1.91 rustc-1.91
git clone https://github.com/Mourty/Weir
cd Weir
PATH=/usr/lib/rust-1.91/bin:$PATH make install
```

</details>

<details>
<summary><b>Debian 13</b></summary>

If `sudo` says you are not in the sudoers file, Debian left it off your
account, which it does when a root password is chosen during
installation. Run `su - -c "usermod -aG sudo $USER"`, give the root
password, and restart the computer.

Debian's own Rust is too old to build Weir, so this gets a current one
from [rustup](https://rustup.rs), the Rust project's installer, which
keeps it in your home folder:

```sh
sudo apt install git curl build-essential clang pkg-config libpipewire-0.3-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh     # press Enter to accept the default
. "$HOME/.cargo/env"
git clone https://github.com/Mourty/Weir
cd Weir
make install
```

</details>

<details>
<summary><b>Arch, CachyOS and other Arch-based systems</b></summary>

```sh
sudo pacman -Syu --needed base-devel clang rust git
git clone https://github.com/Mourty/Weir
cd Weir
make install
```

</details>

**On another system**, Weir needs PipeWire 0.3.77 or newer with
WirePlumber. Install Rust 1.88 or newer, clang, a C compiler, `make`,
`pkg-config` and PipeWire's development files, then run `make install`
from the `Weir` folder.

After `make install`, the `weirctl` command works from your next login.
To update, run `git pull` in the `Weir` folder, then the last line you
installed with again. To remove it, `sudo dnf remove weir` on Fedora and
Nobara, or `make uninstall` in the `Weir` folder elsewhere.

### Then

Open **Weir** from your application menu. To have it ready whenever you
log in, tick **Start Weir when I log in** in Preferences, in the **…**
menu at the top right. Your settings stay in `~/.config/weir`, even if
you remove Weir, until you delete that folder.

## First steps

A new install has a microphone strip, three strips for applications
(Music, Browser, Soundboard), Headset and Speakers buses for what you
hear, and a Stream Mic bus for what others hear.

1. On the **Mic** strip, pick your microphone.
2. On the **Headset** bus, pick your headphones or speakers.
3. In your applications' sound settings, choose "Music (Weir)" or
   "Browser (Weir)" as the output, or move them from the **Apps** menu.
4. In Discord, OBS or your browser, choose **Stream Mic (Weir)** as the
   microphone.

You now hear everything but yourself, and others hear you with everything
you sent them.

## Learn more

| | |
|---|---|
| [User guide](docs/USER_GUIDE.md) | Everything in the window: routing, effects, surround, scenes, preferences, and what to do when something is wrong. |
| [weirctl](docs/CLI.md) | The command line, with a mute key for your keyboard. |
| [Control protocol](docs/API.md) | For Stream Deck plugins, scripts and anything else, with examples in the shell, Python and JavaScript. Ready-made clients are in [examples](examples/). |
| [How Weir works](docs/ARCHITECTURE.md) | The design, for the curious and for contributors. |
| [Contributing](CONTRIBUTING.md) | Building, testing without a sound card, and the rules the code follows. |

## Weir and Voicemeeter

Weir is inspired by [Voicemeeter](https://voicemeeter.com/) from VB-Audio
and borrows its layout: input strips on the left, output buses on the
right, and a column of routing buttons on every strip. The
[user guide](docs/USER_GUIDE.md#coming-from-voicemeeter) shows where
Voicemeeter's bus modes are in Weir. It is an independent project, not
affiliated with or endorsed by VB-Audio, and shares no code with
Voicemeeter, which is a trademark of VB-Audio Software. On Windows, use
Voicemeeter itself; Weir exists because it does not run on Linux.

## Status

Weir 1.0 has been built, installed and tried on Fedora, Nobara, Ubuntu,
Linux Mint, Pop!_OS, Debian and CachyOS. Sound hardware varies endlessly,
so reports of how it does with yours, good or bad, are very welcome: see
[Contributing](CONTRIBUTING.md).

## AI Notice

I want to make it very clear that **this entire project was coded by AI.** 
Specifically, Anthropic’s Claude Code using Opus 5.5. None of this code 
was written by a human. In fact, the only part of this project written 
by a human is this AI usage notice. I simply wanted a piece of software 
that could route audio in this way, and I used AI to make that happen.


## License

Weir is released under the [MIT license](LICENSE). Its noise suppression
comes from [nnnoiseless](https://github.com/jneem/nnnoiseless), a Rust port
of RNNoise, under the BSD 3-clause license.
