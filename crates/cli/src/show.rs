//! What `weirctl` prints for people. (With `--json` it prints the daemon's
//! answers as they are instead.)

use weir_protocol::*;

/// Engine status and a one-line summary.
pub fn status(st: &FullState) {
    let e = &st.engine;
    println!(
        "engine: {} | {} Hz / {} frames | {} strips, {} buses, {} devices, {} apps{}",
        e.state,
        e.sample_rate,
        e.quantum,
        st.mixer.strips.len(),
        st.mixer.buses.len(),
        st.devices.len(),
        st.apps.len(),
        e.error
            .as_ref()
            .map(|x| format!(" | error: {x}"))
            .unwrap_or_default()
    );
}

/// The effects a strip has switched on, for the state table.
pub fn strip_fx(s: &Strip) -> String {
    let fx: Vec<&str> = [
        (s.denoise.enabled, "ns"),
        (s.gate.enabled, "gate"),
        (s.eq.enabled, "eq"),
        (s.compressor.enabled, "comp"),
        (s.ducking.enabled, "duck"),
        (s.insert.enabled, "ext"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if fx.is_empty() {
        "-".into()
    } else {
        fx.join(",")
    }
}

/// The effects a bus has switched on, for the state table.
fn bus_fx(b: &Bus) -> String {
    let mut fx = Vec::new();
    if b.eq.enabled {
        fx.push("eq".to_string());
    }
    if b.limiter.enabled {
        fx.push(format!("lim {:+.0}", b.limiter.ceiling_db));
    }
    if b.insert.enabled {
        fx.push("ext".to_string());
    }
    if fx.is_empty() {
        "-".into()
    } else {
        fx.join(",")
    }
}

/// A downmix in a few words, for what `bus` and `state` print.
pub fn downmix_summary(d: &Downmix) -> String {
    let mut parts = vec![d.method.label().to_lowercase()];
    if !d.method.is_part() {
        parts.push(format!("center {}", mix_level_label(d.center_db)));
        if d.method == DownmixMethod::Standard {
            parts.push(format!(
                "surrounds {}",
                mix_level_label(d.surround_db).to_lowercase()
            ));
        }
        if d.lfe {
            parts.push("subwoofer kept".into());
        }
    }
    parts.join(", ")
}

/// Where a strip's or bus's external effects sit and whether they are
/// connected, for what `strip`, `bus` and `state` print.
fn external_effects(insert: &Insert, target: StripOrBus, st: &FullState) -> String {
    let connected = st.inserts.iter().any(|i| i.target == target && i.connected);
    let status = match (connected, insert.fallback) {
        (true, _) => "connected",
        (false, InsertFallback::PassThrough) => "not connected, so passing the sound through",
        (false, InsertFallback::Silence) => "not connected, so silent",
    };
    format!("{}, {status}", insert.position.label().to_lowercase())
}

/// The devices a strip's or bus's external effects use, named `name`.
fn external_effects_devices(name: &str) -> String {
    let (to, from) = Insert::device_names(name);
    format!("out through '{to}', back through '{from}'")
}

/// A bus name with the strip's send level after it, when it is not 0 dB.
fn with_send(name: String, db: f32) -> String {
    if db.abs() < 0.05 {
        name
    } else {
        format!("{name} ({db:+.1} dB)")
    }
}

/// The buses `s` plays in, with their send levels.
pub fn routes(s: &Strip, mixer: &MixerState) -> Vec<String> {
    s.routes
        .iter()
        .map(|b| {
            let name = mixer
                .bus(*b)
                .map_or_else(|| b.to_string(), |b| b.name.clone());
            with_send(name, s.send_db(*b))
        })
        .collect()
}

/// Every strip and bus, as tables.
pub fn state(st: &FullState) {
    let e = &st.engine;
    println!(
        "Engine: {}{}{}",
        e.state,
        if e.sample_rate > 0 {
            format!(", {} Hz / {} frames", e.sample_rate, e.quantum)
        } else {
            String::new()
        },
        e.error
            .as_ref()
            .map(|x| format!(" ({x})"))
            .unwrap_or_default()
    );
    let mark = |on: bool, text: &str| if on { text } else { "." }.to_string();
    let kind = |k: &dyn std::fmt::Debug| format!("{k:?}").to_lowercase();
    let device = |d: &Option<String>| d.clone().unwrap_or_else(|| "-".into());

    println!("\nSTRIPS");
    let mut t = Table::new(
        &[
            "ID", "Name", "Kind", "Layout", "Gain", "M", "S", "Pan", "Effects", "Routes", "Device",
        ],
        &[0, 4, 7],
    );
    for s in &st.mixer.strips {
        t.row(vec![
            s.id.to_string(),
            s.name.clone(),
            kind(&s.kind),
            s.layout.label(),
            format!("{:+.1} dB", s.gain_db),
            mark(s.mute, "M"),
            mark(s.solo, "S"),
            format!("{:.2}", s.pan),
            strip_fx(s),
            routes(s, &st.mixer).join(", "),
            device(&s.device),
        ]);
    }
    t.print();

    println!("\nBUSES");
    let mut t = Table::new(
        &[
            "ID", "Name", "Kind", "Layout", "Gain", "M", "Mono", "Effects", "Device",
        ],
        &[0, 4],
    );
    for b in &st.mixer.buses {
        t.row(vec![
            b.id.to_string(),
            b.name.clone(),
            kind(&b.kind),
            b.layout.label(),
            format!("{:+.1} dB", b.gain_db),
            mark(b.mute, "M"),
            mark(b.mono, "mono"),
            bus_fx(b),
            device(&b.device),
        ]);
    }
    t.print();
    for b in st.mixer.buses.iter().filter(|b| !b.downmix.is_default()) {
        println!("  {} downmix: {}", b.name, downmix_summary(&b.downmix));
    }
    let strips = st
        .mixer
        .strips
        .iter()
        .filter(|s| s.insert.enabled)
        .map(|s| {
            let at = external_effects(&s.insert, StripOrBus::Strip(s.id), st);
            format!("  {}: {at}", s.name)
        });
    let buses = st.mixer.buses.iter().filter(|b| b.insert.enabled).map(|b| {
        let at = external_effects(&b.insert, StripOrBus::Bus(b.id), st);
        format!("  {}: {at}", b.name)
    });
    let lines: Vec<String> = strips.chain(buses).collect();
    if !lines.is_empty() {
        println!("\nEXTERNAL EFFECTS\n{}", lines.join("\n"));
    }
    system_volumes(st);
}

/// The system's own volume on our devices, which Weir never sets, when it
/// is turning any of them down.
fn system_volumes(st: &FullState) {
    let describe = |v: &SystemVolume| {
        if v.mute {
            "muted".to_string()
        } else {
            format!("at {:.0}% ({:.1} dB)", v.percent(), v.volume_db)
        }
    };
    let sv = &st.system_volumes;
    let strips = sv.strips.iter().filter_map(|(id, v)| {
        let s = st.mixer.strip(*id)?;
        v.is_reducing()
            .then(|| format!("{} {}", s.name, describe(v)))
    });
    let buses = sv.buses.iter().filter_map(|(id, v)| {
        let b = st.mixer.bus(*id)?;
        v.is_reducing()
            .then(|| format!("{} {}", b.name, describe(v)))
    });
    let notes: Vec<String> = strips.chain(buses).collect();
    if !notes.is_empty() {
        println!();
        println!(
            "The system's volume control has turned these devices down: {}",
            notes.join(", ")
        );
    }
}

/// A strip after a change, with a line for each effect that is on.
pub fn strip(s: &Strip, st: &FullState) {
    println!(
        "strip {} '{}': {:+.1} dB{}{} pan {:.2}, effects: {}",
        s.id,
        s.name,
        s.gain_db,
        if s.mute { " MUTE" } else { "" },
        if s.solo { " SOLO" } else { "" },
        s.pan,
        strip_fx(s)
    );
    if s.gate.enabled {
        let g = &s.gate;
        println!(
            "  gate: opens above {:.0} dB, closes to {:.0} dB; attack {} ms, hold {} ms, release {} ms",
            g.threshold_db, g.range_db, g.attack_ms, g.hold_ms, g.release_ms
        );
    }
    if s.compressor.enabled {
        let c = &s.compressor;
        println!(
            "  compressor: {}:1 above {:.0} dB, attack {} ms, release {} ms, lift {:+.1} dB{}",
            c.ratio,
            c.threshold_db,
            c.attack_ms,
            c.release_ms,
            c.makeup(),
            if c.auto_makeup { " (auto)" } else { "" }
        );
    }
    if s.ducking.enabled {
        let d = &s.ducking;
        let names = |ids: Vec<String>| {
            if ids.is_empty() {
                "nothing".to_string()
            } else {
                ids.join(", ")
            }
        };
        let m = &st.mixer;
        let when = names(
            d.triggers
                .iter()
                .map(|t| m.strip(*t).map_or(t.to_string(), |s| s.name.clone()))
                .collect(),
        );
        let mixes = if d.buses.is_empty() {
            "every mix".to_string()
        } else {
            names(
                d.buses
                    .iter()
                    .map(|b| m.bus(*b).map_or(b.to_string(), |b| b.name.clone()))
                    .collect(),
            )
        };
        println!(
            "  ducking: down {:.0} dB in {mixes} while {when} is heard (above {:.0} dB)",
            d.amount_db, d.threshold_db
        );
    }
    if s.upmix != Upmix::Off || s.subwoofer {
        println!(
            "  upmix: {}{}",
            s.upmix.label().to_lowercase(),
            if s.subwoofer {
                ", bass on the subwoofer"
            } else {
                ""
            }
        );
    }
    if s.insert.enabled {
        let at = external_effects(&s.insert, StripOrBus::Strip(s.id), st);
        println!("  external effects: {at}");
        println!("    {}", external_effects_devices(&s.name));
    }
    if s.denoise.enabled {
        println!("  noise suppression: {:.0}%", s.denoise.amount * 100.0);
        if st.engine.sample_rate != 0 && st.engine.sample_rate != Denoise::SAMPLE_RATE {
            println!(
                "  note: noise suppression only works at 48000 Hz; the engine runs at {} Hz",
                st.engine.sample_rate
            );
        }
    }
}

/// A bus after a change.
pub fn bus(b: &Bus, st: &FullState) {
    println!(
        "bus {} '{}': {:+.1} dB{}{}{}{}",
        b.id,
        b.name,
        b.gain_db,
        if b.mute { " MUTE" } else { "" },
        if b.mono { " MONO" } else { "" },
        if b.eq.enabled { " EQ" } else { "" },
        if b.limiter.enabled {
            format!(" limiter at {:+.1} dB", b.limiter.ceiling_db)
        } else {
            String::new()
        }
    );
    if !b.downmix.is_default() {
        println!("  downmix: {}", downmix_summary(&b.downmix));
    }
    if b.insert.enabled {
        let at = external_effects(&b.insert, StripOrBus::Bus(b.id), st);
        println!("  external effects: {at}");
        println!("    {}", external_effects_devices(&b.name));
    }
}

/// The devices hardware strips and buses can use.
pub fn devices(devices: &[DeviceInfo]) {
    let mut t = Table::new(&["Kind", "Name (use this)", "Channels", "Description"], &[]);
    for d in devices {
        let channels: Vec<&str> = d.channels.iter().map(|p| p.as_str()).collect();
        t.row(vec![
            format!("{:?}", d.kind).to_lowercase(),
            d.name.clone(),
            channels.join(" "),
            d.description.clone(),
        ]);
    }
    t.print();
}

/// The applications playing sound.
pub fn apps(apps: &[AppStream], mixer: &MixerState) {
    if apps.is_empty() {
        println!("(no application is playing sound)");
        return;
    }
    let mut t = Table::new(
        &["ID", "Application", "Strip", "Volume", "Plays to"],
        &[0, 3],
    );
    for a in apps {
        let volume = match (a.volume_db, a.mute) {
            (_, true) => "muted".to_string(),
            (Some(db), false) => format!("{db:+.1} dB"),
            (None, false) => "-".to_string(),
        };
        t.row(vec![
            a.id.to_string(),
            a.name.clone(),
            a.strip
                .and_then(|id| mixer.strip(id))
                .map_or("-".into(), |s| s.name.clone()),
            volume,
            match (&a.target, &a.media_name) {
                (Some(t), Some(m)) => format!("{t} [{m}]"),
                (Some(t), None) => t.clone(),
                (None, _) => "(nowhere)".into(),
            },
        ]);
    }
    t.print();
}

/// The application rules, marking the ones whose application is playing.
pub fn rules(st: &FullState) {
    if st.app_rules.is_empty() {
        println!("(no rules)");
    }
    for r in &st.app_rules {
        let to = match r.strip {
            None => "leave alone".to_string(),
            Some(id) => st
                .mixer
                .strip(id)
                .map_or(format!("strip {id} (removed)"), |s| s.name.clone()),
        };
        let playing = if st.apps.iter().any(|a| r.matches(a)) {
            "  (playing)"
        } else {
            ""
        };
        println!("{:<24} -> {to}{playing}", r.app);
    }
}

/// The daemon's settings, in words.
pub fn settings(settings: &Settings, st: &FullState) {
    let held = |v: Option<u32>, unit: &str| {
        v.map_or("PipeWire decides".to_string(), |v| {
            format!("held at {v} {unit}")
        })
    };
    let solo = match settings.solo {
        SoloMode::Exclusive => "silences the other strips in every mix".to_string(),
        SoloMode::Cue(b) => format!(
            "cue on {}",
            st.mixer.bus(b).map_or(b.to_string(), |x| x.name.clone())
        ),
    };
    let tray = if settings.tray {
        settings.tray_icon.label().to_lowercase()
    } else {
        "off".into()
    };
    println!("sample rate:  {}", held(settings.sample_rate, "Hz"));
    println!("buffer size:  {}", held(settings.quantum, "frames"));
    println!("solo:         {solo}");
    println!("meter rate:   {} per second", settings.meter_rate_hz);
    println!("startup:      {}", settings.startup.label().to_lowercase());
    let login = match settings.start_at_login {
        Some(true) => "yes",
        Some(false) => "no",
        None => "not available (Weir's service is not installed)",
    };
    println!("at login:     {login}");
    println!("tray icon:    {tray}");
    println!(
        "running at:   {} Hz, {} frames",
        st.engine.sample_rate, st.engine.quantum
    );
}

/// What can be undone and redone, the next undo marked.
pub fn history(h: &HistoryInfo) {
    if h.undo.is_empty() && h.redo.is_empty() {
        println!("(nothing to undo or redo)");
    }
    for e in h.redo.iter().rev() {
        println!("  redo  {}", e.label);
    }
    for (k, e) in h.undo.iter().enumerate() {
        println!("{} undo  {}", if k == 0 { ">" } else { " " }, e.label);
    }
}

/// An equalizer's bands, one per line.
pub fn bands(bands: &[EqBand]) {
    if bands.is_empty() {
        println!("  (no bands)");
    }
    for (i, b) in bands.iter().enumerate() {
        let gain = if b.kind.uses_gain() {
            format!("{:+5.1} dB", b.gain_db)
        } else {
            "        ".into()
        };
        println!(
            "  {:>2}. {:<11} {:>7.0} Hz  {}  Q {:<5.2}{}",
            i + 1,
            b.kind.label(),
            b.freq_hz,
            gain,
            b.q,
            if b.enabled { "" } else { "  (off)" }
        );
    }
}

/// The equalizer presets, noting which are the user's own.
pub fn eq_presets(presets: &[EqPreset]) {
    for p in presets {
        println!(
            "{:<30} {} band{}{}",
            p.name,
            p.bands.len(),
            if p.bands.len() == 1 { "" } else { "s" },
            if p.builtin { "" } else { "  (yours)" }
        );
    }
}

/// One `meters` notification, on a line or a few: peaks per strip and
/// bus, and what the limiters, compressors, ducking and gates are doing.
pub fn meters(m: &Meters) {
    let fmt = |v: &[f32]| {
        v.iter()
            .map(|x| format!("{x:6.1}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let line = |label: &str, items: Vec<String>| {
        if !items.is_empty() {
            println!("{label} {}", items.join(", "));
        }
    };
    line(
        "limiters",
        m.limiters
            .iter()
            .filter(|(_, db)| **db < -0.05)
            .map(|(id, db)| format!("b{id} limiting {db:.1} dB"))
            .collect(),
    );
    line(
        "compressors",
        m.compressors
            .iter()
            .map(|(id, c)| {
                format!(
                    "s{id} in {:.0} dB, turning down {:.1} dB",
                    c.level_db, -c.reduction_db
                )
            })
            .collect(),
    );
    line(
        "ducking",
        m.ducking
            .iter()
            .filter(|(_, db)| **db < -0.05)
            .map(|(id, db)| format!("s{id} down {:.1} dB", -db))
            .collect(),
    );
    let strips: Vec<String> = m
        .strips
        .iter()
        .map(|(id, v)| format!("s{id}[{}]", fmt(v)))
        .collect();
    let buses: Vec<String> = m
        .buses
        .iter()
        .map(|(id, v)| format!("b{id}[{}]", fmt(v)))
        .collect();
    let gates: Vec<String> = m
        .gates
        .iter()
        .map(|(id, g)| {
            let state = if g.reduction_db > -0.5 {
                "open".to_string()
            } else {
                format!("turning down {:.0} dB", -g.reduction_db)
            };
            format!("s{id} in {:.0} dB, {state}", g.level_db)
        })
        .collect();
    if gates.is_empty() {
        println!("meters {} | {}", strips.join(" "), buses.join(" "));
    } else {
        println!(
            "meters {} | {} | gates {}",
            strips.join(" "),
            buses.join(" "),
            gates.join(", ")
        );
    }
}

/// The hotkeys, what they do, and what is wrong with any of them.
pub fn hotkeys(info: &HotkeysInfo, mixer: &MixerState) {
    println!("{}", info.keys.message);
    if info.hotkeys.is_empty() {
        println!("(no hotkeys)");
        return;
    }
    println!();
    let mut t = Table::new(&["ID", "Name", "Keys", "What it does"], &[0]);
    for h in &info.hotkeys {
        let keys = match (info.keys.assigned.get(&h.id), &h.keys) {
            (Some(given), _) => given.clone(),
            (None, Some(keys)) => keys.clone(),
            (None, None) => "-".into(),
        };
        let keys = if h.enabled {
            keys
        } else {
            format!("{keys} (off)")
        };
        t.row(vec![
            h.id.to_string(),
            h.name.clone(),
            keys,
            describe_hotkey(h, mixer),
        ]);
    }
    t.print();
    for p in &info.problems {
        let name = info
            .hotkeys
            .iter()
            .find(|h| h.id == p.hotkey)
            .map_or("Hotkeys", |h| h.name.as_str());
        println!("{name}: {}", p.problem);
    }
}

/// Columns lined up to their widest entry, for the listings.
struct Table {
    rows: Vec<Vec<String>>,
    /// The columns aligned to the right, such as numbers.
    right: Vec<usize>,
}

impl Table {
    fn new(headers: &[&str], right: &[usize]) -> Self {
        Self {
            rows: vec![headers.iter().map(|h| h.to_string()).collect()],
            right: right.to_vec(),
        }
    }

    fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    /// The table as text, one line per row, with two spaces between
    /// columns and none after the last.
    fn render(&self) -> String {
        let columns = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..columns)
            .map(|c| {
                self.rows
                    .iter()
                    .filter_map(|r| r.get(c))
                    .map(|cell| cell.chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let mut out = String::new();
        for row in &self.rows {
            let cells: Vec<String> = row
                .iter()
                .zip(&widths)
                .enumerate()
                .map(|(c, (cell, &w))| {
                    if self.right.contains(&c) {
                        format!("{cell:>w$}")
                    } else {
                        format!("{cell:<w$}")
                    }
                })
                .collect();
            out.push_str(cells.join("  ").trim_end());
            out.push('\n');
        }
        out
    }

    fn print(&self) {
        print!("{}", self.render());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_line_up_whatever_the_width() {
        let mut t = Table::new(&["ID", "Name", "Kind"], &[0]);
        t.row(vec!["1".into(), "Mic".into(), "hardware".into()]);
        t.row(vec![
            "12".into(),
            "A very long name".into(),
            "virtual".into(),
        ]);
        assert_eq!(
            t.render(),
            "ID  Name              Kind\n 1  Mic               hardware\n12  A very long name  virtual\n"
        );
    }
}
