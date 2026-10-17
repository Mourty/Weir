# mp3lame-sys (vendored)

A copy of [mp3lame-sys](https://github.com/DoumanAsh/mp3lame-sys) 0.1.11,
the bindings to LAME that `mp3lame-encoder` uses, patched in from the
workspace's `Cargo.toml`.

The only change is how LAME is built. The published crate runs LAME's
`configure` script on Linux, and its libtool step fails when the folder's
path contains a space (`/home/me/My Projects/Weir/...`). This copy compiles
LAME with the `cc` crate on every platform: Visual C++ uses LAME's own
`configMS.h` with the same settings as the published crate, and everything
else uses `config/config.h`, which sets what `configure` finds on Linux.

Only the LAME 3.100 sources the encoder needs are kept; the decoder,
frontend and build files are left out.

LAME and these bindings are under the LGPL (see `LICENSE` and
`lame-3.100/COPYING`).
Weir installs LAME's license beside its own, as `LAME-COPYING.txt`.
