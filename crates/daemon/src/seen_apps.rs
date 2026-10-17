//! Applications Weir has heard play, kept so rules can be made for ones
//! that play only now and then (`seen_apps` in the protocol). They live in
//! a file of their own: the list belongs to this computer, so it is not
//! part of the configuration, its backups or an export.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use weir_protocol::{AppStream, MixerState, SeenApp, TargetKind, SEEN_APPS_DAYS, SEEN_APPS_MAX};

/// The version of the file's layout.
const VERSION: u32 = 1;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// The file: `{"version": 1, "apps": [...]}`.
#[derive(Serialize, Deserialize)]
struct SeenFile {
    version: u32,
    #[serde(default)]
    apps: Vec<SeenApp>,
}

/// The applications heard lately, most recently heard first.
#[derive(Debug, Default)]
pub struct SeenApps {
    apps: Vec<SeenApp>,
}

/// Now, in milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl SeenApps {
    /// The list kept at `path`, without the applications heard too long
    /// ago. A file that cannot be read is copied into `backups` and the
    /// list starts afresh: it is only a list of names, and losing it costs
    /// less than a daemon that will not start.
    pub fn load(path: &Path, backups: &Path, now_ms: u64) -> Self {
        let mut seen = match read(path) {
            Ok(apps) => Self { apps },
            Err(e) => {
                warn!("{e:#}; starting a new list of applications heard");
                let copy = backups.join("seen-apps-unreadable.json");
                if let Err(e) = std::fs::create_dir_all(backups)
                    .and_then(|_| std::fs::copy(path, &copy).map(|_| ()))
                {
                    warn!("could not copy it to {}: {e}", copy.display());
                }
                Self::default()
            }
        };
        seen.tidy(now_ms);
        seen
    }

    /// Write the list to `path`.
    pub fn save(&self, path: &Path) -> Result<()> {
        let file = SeenFile {
            version: VERSION,
            apps: self.apps.clone(),
        };
        let text = serde_json::to_string_pretty(&file).expect("the list serializes");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        // Written beside it and moved over it, so a crash never leaves half
        // a file.
        let part = path.with_extension("json.part");
        std::fs::write(&part, text).with_context(|| format!("writing {}", part.display()))?;
        std::fs::rename(&part, path).with_context(|| format!("writing {}", path.display()))
    }

    /// The applications, most recently heard first.
    pub fn list(&self) -> &[SeenApp] {
        &self.apps
    }

    /// Take in that the applications playing were `before` and are `now`.
    /// Returns whether the list changed in a way clients would show: an
    /// application started or stopped playing, or plays elsewhere. A
    /// volume change, which also arrives as a new list, changes nothing.
    pub fn note(
        &mut self,
        before: &[AppStream],
        now: &[AppStream],
        mixer: &MixerState,
        now_ms: u64,
    ) -> bool {
        let mut changed = false;
        for (name, streams) in by_name(now) {
            let started = !before.iter().any(|a| same(&a.name, name));
            let binary = streams.iter().find_map(|a| a.binary.clone());
            // Where it plays, as a strip of Weir's or somewhere else. A
            // stream connected nowhere, as one is for a moment when it
            // starts or ends, says nothing about that.
            let strip = streams
                .iter()
                .find_map(|a| a.strip.and_then(|id| mixer.strip(id)))
                .map(|s| Some(s.name.clone()))
                .or_else(|| streams.iter().any(|a| a.target.is_some()).then_some(None));
            match self.apps.iter_mut().find(|s| same(&s.name, name)) {
                Some(seen) => {
                    if started {
                        seen.last_seen_ms = now_ms;
                        changed = true;
                    }
                    if binary.is_some() && seen.binary != binary {
                        seen.binary = binary;
                        changed = true;
                    }
                    if let Some(strip) = strip {
                        if seen.strip != strip {
                            seen.strip = strip;
                            changed = true;
                        }
                    }
                }
                None => {
                    self.apps.push(SeenApp {
                        name: name.to_string(),
                        binary,
                        last_seen_ms: now_ms,
                        strip: strip.flatten(),
                    });
                    changed = true;
                }
            }
        }
        for seen in &mut self.apps {
            let was = before.iter().any(|a| same(&a.name, &seen.name));
            let is = now.iter().any(|a| same(&a.name, &seen.name));
            if was && !is {
                seen.last_seen_ms = now_ms;
                changed = true;
            }
        }
        self.tidy(now_ms) || changed
    }

    /// Weir stops hearing anything: the applications `playing` stopped as
    /// far as the list goes.
    pub fn stop_all(&mut self, playing: &[AppStream], now_ms: u64) {
        for seen in &mut self.apps {
            if playing.iter().any(|a| same(&a.name, &seen.name)) {
                seen.last_seen_ms = now_ms;
            }
        }
        self.tidy(now_ms);
    }

    /// Take the application called `name` off the list. One of those
    /// `playing` stays: it would only come back with the next change.
    pub fn forget(&mut self, name: &str, playing: &[AppStream]) -> Result<()> {
        if playing.iter().any(|a| same(&a.name, name)) {
            bail!(
                "'{}' is playing now; it can be forgotten once it stops",
                name.trim()
            );
        }
        let before = self.apps.len();
        self.apps.retain(|s| !same(&s.name, name));
        if self.apps.len() == before {
            bail!("no application called '{}' was heard lately", name.trim());
        }
        Ok(())
    }

    /// Forget every application but those `playing`. Returns whether any
    /// went.
    pub fn clear(&mut self, playing: &[AppStream]) -> bool {
        let before = self.apps.len();
        self.apps
            .retain(|s| playing.iter().any(|a| same(&a.name, &s.name)));
        self.apps.len() != before
    }

    /// Strips were renamed: where applications last played takes the new
    /// names. Returns whether any did.
    pub fn follow_renames(&mut self, renamed: &[(TargetKind, String, String)]) -> bool {
        let mut changed = false;
        for seen in &mut self.apps {
            let Some(strip) = &mut seen.strip else {
                continue;
            };
            if let Some((_, _, new)) = renamed
                .iter()
                .find(|(k, old, _)| *k == TargetKind::Strip && same(old, strip))
            {
                *strip = new.clone();
                changed = true;
            }
        }
        changed
    }

    /// Newest first, nothing older than [`SEEN_APPS_DAYS`], at most
    /// [`SEEN_APPS_MAX`]. Returns whether any went.
    fn tidy(&mut self, now_ms: u64) -> bool {
        let before = self.apps.len();
        let oldest = now_ms.saturating_sub(SEEN_APPS_DAYS * DAY_MS);
        self.apps
            .retain(|s| s.last_seen_ms >= oldest && !s.name.trim().is_empty());
        self.apps.sort_by_key(|s| std::cmp::Reverse(s.last_seen_ms));
        self.apps.truncate(SEEN_APPS_MAX);
        self.apps.len() != before
    }
}

/// Whether two application names are the same, ignoring case and spaces
/// around them.
fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The streams in `apps` grouped by application name, in the order the
/// names first come: a browser playing in two tabs is one application.
fn by_name(apps: &[AppStream]) -> Vec<(&str, Vec<&AppStream>)> {
    let mut groups: Vec<(&str, Vec<&AppStream>)> = Vec::new();
    for a in apps {
        let name = a.name.trim();
        if name.is_empty() {
            continue;
        }
        match groups.iter_mut().find(|(n, _)| same(n, name)) {
            Some((_, streams)) => streams.push(a),
            None => groups.push((name, vec![a])),
        }
    }
    groups
}

/// The list at `path`; none when there is no file.
fn read(path: &Path) -> Result<Vec<SeenApp>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: SeenFile =
        serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    if file.version > VERSION {
        bail!(
            "{} was written by a newer Weir (version {})",
            path.display(),
            file.version
        );
    }
    Ok(file.apps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use weir_protocol::{ChannelLayout, Strip, StripKind};

    const NOW: u64 = 1_800_000_000_000;

    fn app(id: u32, name: &str, strip: Option<u32>) -> AppStream {
        AppStream {
            id,
            name: name.into(),
            binary: Some(name.to_lowercase()),
            media_name: None,
            target: Some(match strip {
                Some(s) => format!("weir.input.{s}"),
                None => "alsa_output.speakers".into(),
            }),
            strip,
            volume_db: None,
            mute: false,
        }
    }

    fn mixer() -> MixerState {
        let mut m = MixerState::default();
        m.strips.push(Strip::new(
            3,
            "Music",
            StripKind::Virtual,
            ChannelLayout::Stereo,
        ));
        m
    }

    #[test]
    fn apps_are_kept_newest_first_with_where_they_played() {
        let m = mixer();
        let mut seen = SeenApps::default();
        let one = vec![app(40, "Spotify", Some(3))];
        assert!(seen.note(&[], &one, &m, NOW));
        let two = vec![app(40, "Spotify", Some(3)), app(41, "Firefox", None)];
        assert!(seen.note(&one, &two, &m, NOW + 1000));
        let names: Vec<&str> = seen.list().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Firefox", "Spotify"]);
        assert_eq!(seen.list()[1].strip.as_deref(), Some("Music"));
        assert_eq!(seen.list()[0].strip, None);
        assert_eq!(seen.list()[0].binary.as_deref(), Some("firefox"));

        // A volume change: the same streams again.
        let mut louder = two.clone();
        louder[0].volume_db = Some(-3.0);
        assert!(!seen.note(&two, &louder, &m, NOW + 2000));

        // Spotify stops: heard last just now.
        let three = vec![app(41, "Firefox", None)];
        assert!(seen.note(&louder, &three, &m, NOW + 5000));
        assert_eq!(seen.list()[0].name, "Spotify");
        assert_eq!(seen.list()[0].last_seen_ms, NOW + 5000);
    }

    #[test]
    fn a_stream_connected_nowhere_keeps_the_strip_it_was_on() {
        let m = mixer();
        let mut seen = SeenApps::default();
        let on = vec![app(40, "Spotify", Some(3))];
        seen.note(&[], &on, &m, NOW);
        let mut leaving = on.clone();
        leaving[0].strip = None;
        leaving[0].target = None;
        assert!(!seen.note(&on, &leaving, &m, NOW + 10));
        assert_eq!(seen.list()[0].strip.as_deref(), Some("Music"));
        // Playing straight to a device is somewhere else.
        let direct = vec![app(40, "Spotify", None)];
        assert!(seen.note(&leaving, &direct, &m, NOW + 20));
        assert_eq!(seen.list()[0].strip, None);
    }

    #[test]
    fn one_application_in_two_streams_is_one_entry() {
        let m = mixer();
        let mut seen = SeenApps::default();
        let tabs = vec![app(40, "Firefox", None), app(41, "firefox", Some(3))];
        seen.note(&[], &tabs, &m, NOW);
        assert_eq!(seen.list().len(), 1);
        assert_eq!(seen.list()[0].strip.as_deref(), Some("Music"));
        // Noting the same streams again changes nothing.
        assert!(!seen.note(&tabs, &tabs, &m, NOW + 10));
        // One tab closes: still playing.
        let one = vec![tabs[0].clone()];
        seen.note(&tabs, &one, &m, NOW + 20);
        assert_eq!(seen.list()[0].last_seen_ms, NOW);
    }

    #[test]
    fn old_and_extra_apps_go() {
        let m = mixer();
        let mut seen = SeenApps::default();
        let old = vec![app(1, "Old", None)];
        seen.note(&[], &old, &m, NOW);
        seen.note(&old, &[], &m, NOW);
        let later = NOW + (SEEN_APPS_DAYS + 1) * DAY_MS;
        let new = vec![app(2, "New", None)];
        assert!(seen.note(&[], &new, &m, later));
        let names: Vec<&str> = seen.list().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["New"]);

        let mut many = SeenApps::default();
        for k in 0..SEEN_APPS_MAX + 5 {
            let a = vec![app(k as u32, &format!("App {k}"), None)];
            many.note(&[], &a, &m, NOW + k as u64);
        }
        assert_eq!(many.list().len(), SEEN_APPS_MAX);
        assert_eq!(
            many.list().last().unwrap().name,
            "App 5",
            "the ones heard longest ago went"
        );
    }

    #[test]
    fn forgetting_clearing_and_renames() {
        let m = mixer();
        let mut seen = SeenApps::default();
        let playing = vec![app(40, "Spotify", Some(3)), app(41, "Firefox", None)];
        seen.note(&[], &playing, &m, NOW);
        assert!(seen.follow_renames(&[(TargetKind::Strip, "music".into(), "Tunes".into())]));
        let spotify = seen.list().iter().find(|s| s.name == "Spotify").unwrap();
        assert_eq!(spotify.strip.as_deref(), Some("Tunes"));
        assert!(!seen.follow_renames(&[(TargetKind::Bus, "Tunes".into(), "X".into())]));

        // One playing now stays, whatever is asked.
        let e = seen.forget("Spotify", &playing).unwrap_err().to_string();
        assert_eq!(
            e,
            "'Spotify' is playing now; it can be forgotten once it stops"
        );
        assert!(!seen.clear(&playing));
        assert_eq!(seen.list().len(), 2);

        seen.forget(" spotify ", &[]).unwrap();
        assert_eq!(seen.list().len(), 1);
        let e = seen.forget("Spotify", &[]).unwrap_err().to_string();
        assert_eq!(e, "no application called 'Spotify' was heard lately");
        assert!(seen.clear(&[]));
        assert!(!seen.clear(&[]));
    }

    #[test]
    fn the_list_is_saved_and_read_back() {
        let dir = std::env::temp_dir().join(format!("weir-seen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("seen-apps.json");
        let backups = dir.join("backups");
        let m = mixer();
        let mut seen = SeenApps::default();
        let playing = vec![app(40, "Spotify", Some(3))];
        seen.note(&[], &playing, &m, NOW);
        seen.stop_all(&playing, NOW + 60_000);
        seen.save(&path).unwrap();
        let back = SeenApps::load(&path, &backups, NOW + 120_000);
        assert_eq!(back.list(), seen.list());
        assert_eq!(back.list()[0].last_seen_ms, NOW + 60_000);

        // A broken file is kept aside, and the list starts afresh.
        std::fs::write(&path, "{ not json").unwrap();
        let fresh = SeenApps::load(&path, &backups, NOW);
        assert!(fresh.list().is_empty());
        assert!(backups.join("seen-apps-unreadable.json").exists());
        // No file at all is an empty list.
        let none = SeenApps::load(&dir.join("none.json"), &backups, NOW);
        assert!(none.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
