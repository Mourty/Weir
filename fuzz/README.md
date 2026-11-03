# Fuzzing

What reaches Weir from outside, made up and damaged millions of times
over, to find anything that crashes it, hangs it or takes far too much
memory. Bad input should only ever be an error about that input.

| Target   | What it does with each input                                                     | Seeds           |
| -------- | -------------------------------------------------------------------------------- | --------------- |
| `rpc`    | reads it as lines to the control socket, and runs each request on a mixer of its own, as `weirctl`, the window and plugins do | `seeds/rpc`     |
| `import` | reads it as a `.zip` or `.json` to import settings from, looks at it as the Import window does, and imports it | `seeds/import`  |
| `config` | takes it for one of Weir's own files (`config.toml`, `hotkeys.json`, meter colors, equalizer presets, applications heard, a saved setup or scene), starts from it and uses it | `seeds/config`  |
| `sound`  | decodes it as a hotkey sound (`.wav`, `.mp3`, `.ogg`, `.flac`)                    | `seeds/sound`   |
| `text`   | reads it as other text from outside: keys typed for a hotkey, a speaker layout, a recordings' name pattern, a color, versions, GitHub's answer about releases, the session's environment, a crash report to hide personal details in, names, a hotkey, a scene, a setup, a request's strips and buses | `seeds/text`    |
| `mixer`  | makes a mixer from it, starting from Weir's first one with every effect on, and plays it with the mixing code, before and after a change | `seeds/mixer`   |

For `import`, `config`, `sound` and `text` the first byte picks what the
rest is taken for (the lists are in `src/lib.rs`). `mixer` reads its
bytes as the numbers, switches and layouts of the mixer's settings, so
any bytes make a mixer.

Besides not crashing, the targets check what must always hold: a mixer
Weir keeps is as `normalize` leaves it, and goes through JSON and back
unchanged (so no infinity or NaN can get into it, which JSON cannot
carry); every answer to a request can be written; the mixing code makes
only numbers, never NaN or infinity; keys and colors read back as they
were written; a recording's file names stay inside its folder.

## Running

It needs Rust nightly and cargo-fuzz:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
```

Then, from the top of the repository:

```sh
make fuzz TARGET=import              # ten minutes of one target
make fuzz TARGET=mixer SECONDS=3600
```

Or by hand (`corpus/` keeps the inputs that reached new code, so the next
run starts from them):

```sh
cargo +nightly fuzz run -O import fuzz/corpus/import fuzz/seeds/import -- \
    -max_total_time=600 -timeout=10 -rss_limit_mb=2048 -malloc_limit_mb=4200
```

An input that crashed it is saved in `fuzz/artifacts/<target>/`; run the
target on that input alone to see why:

```sh
cargo +nightly fuzz run -O import fuzz/artifacts/import/crash-...
```

## What counts

- A panic in Weir's own code (`crates/`) is a crash. One in other
  people's crates isn't, where Weir reads files with them (sound files
  with Symphonia, exports with the zip crate): Weir catches those
  (`weir_protocol::crash::guarded`) and says the file is damaged, as it
  does when it runs. A panic inside the standard library (a slice index
  out of range, say) belongs to the first code outside it that called
  it. `WEIR_FUZZ_ALL=1` counts other people's panics too. Requests, Weir's
  own files and other text aren't read behind a guard, so for `rpc`,
  `config` and `text` any panic counts.
- Arithmetic that overflows in Weir's own crates is a crash here, though
  the release build lets it wrap around (`[profile.release.package.*]`
  in `Cargo.toml`).
- Asking for more than 4200 MB at once, using more than 2 GB, or taking
  more than ten seconds over one input is a crash too.
- Memory never freed is reported at the end of a run, except for `mixer`
  (`-detect_leaks=0`, which `make fuzz` adds): the noise suppressor's FFT
  library keeps a cache for each thread for good, 22 KB once, which the
  check would report every time. Memory that grows with each input still
  shows, as the 2 GB limit.

The `rpc` target leaves out requests that reach files outside its own
folder or other programs (exports, imports, adding sounds, recording,
diagnostics, restarting); `import` covers imports. It never runs
`systemctl`: starting at login is kept nowhere.

When the fix for one is in, a copy of the input goes in a test: a damaged
sound in `crates/daemon/testdata/bad/`, which `cargo test` decodes every
time, or a unit test beside the code that was wrong.
