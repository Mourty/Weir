//! Scenes and setups: the two kinds of saved mixer state.
//!
//! A **setup** is the mixer's shape: which strips and buses there are, the
//! devices they use, their layouts, names, colors and order. It is saved as
//! a whole [`MixerState`], so it is also a full snapshot.
//!
//! A **scene** is only the mix: levels, mutes, routes, send levels and
//! effects. Loading one never changes which devices are used, so the same
//! "Streaming" or "Late night" scene works on any setup that has strips and
//! buses of the same names.

use crate::fx::{Compressor, Denoise, Ducking, Equalizer, Gate, Limiter};
use crate::model::{Bus, BusId, Downmix, MixerState, Strip, StripId, Upmix};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The mix of one strip, as a scene keeps it: the [`Strip`] settings that
/// are not about its device or its place in the mixer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StripMix {
    /// The strip's id when the scene was saved.
    pub id: StripId,
    /// The strip's name when the scene was saved, which is how a scene finds
    /// it again.
    pub name: String,
    /// See [`Strip::gain_db`].
    #[serde(default)]
    pub gain_db: f32,
    /// See [`Strip::mute`].
    #[serde(default)]
    pub mute: bool,
    /// See [`Strip::solo`].
    #[serde(default)]
    pub solo: bool,
    /// See [`Strip::pan`].
    #[serde(default)]
    pub pan: f32,
    /// See [`Strip::routes`].
    #[serde(default)]
    pub routes: BTreeSet<BusId>,
    /// See [`Strip::sends`].
    #[serde(
        default,
        deserialize_with = "crate::model::id_keys::deserialize",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub sends: BTreeMap<BusId, f32>,
    /// See [`Strip::upmix`].
    #[serde(default, alias = "spread", skip_serializing_if = "Upmix::is_default")]
    pub upmix: Upmix,
    /// See [`Strip::subwoofer`].
    #[serde(default)]
    pub subwoofer: bool,
    /// See [`Strip::denoise`].
    #[serde(default, skip_serializing_if = "Denoise::is_default")]
    pub denoise: Denoise,
    /// See [`Strip::gate`].
    #[serde(default, skip_serializing_if = "Gate::is_default")]
    pub gate: Gate,
    /// See [`Strip::eq`].
    #[serde(default, skip_serializing_if = "Equalizer::is_default")]
    pub eq: Equalizer,
    /// See [`Strip::compressor`].
    #[serde(default, skip_serializing_if = "Compressor::is_default")]
    pub compressor: Compressor,
    /// See [`Strip::ducking`].
    #[serde(default, skip_serializing_if = "Ducking::is_default")]
    pub ducking: Ducking,
}

impl StripMix {
    fn of(s: &Strip) -> Self {
        Self {
            id: s.id,
            name: s.name.clone(),
            gain_db: s.gain_db,
            mute: s.mute,
            solo: s.solo,
            pan: s.pan,
            routes: s.routes.clone(),
            sends: s.sends.clone(),
            upmix: s.upmix,
            subwoofer: s.subwoofer,
            denoise: s.denoise,
            gate: s.gate,
            eq: s.eq.clone(),
            compressor: s.compressor,
            ducking: s.ducking.clone(),
        }
    }

    /// Put this mix onto `s`. Routes, send levels and ducking name buses and
    /// strips by their ids when the scene was saved; `bus_ids` and
    /// `strip_ids` turn those into the ids they have now, and anything they
    /// do not know is dropped.
    fn apply(
        &self,
        s: &mut Strip,
        bus_ids: &BTreeMap<BusId, BusId>,
        strip_ids: &BTreeMap<StripId, StripId>,
    ) {
        let bus = |b: &BusId| bus_ids.get(b).copied();
        s.gain_db = self.gain_db;
        s.mute = self.mute;
        s.solo = self.solo;
        s.pan = self.pan;
        s.routes = self.routes.iter().filter_map(bus).collect();
        s.sends = self
            .sends
            .iter()
            .filter_map(|(b, db)| Some((bus(b)?, *db)))
            .collect();
        s.upmix = self.upmix;
        s.subwoofer = self.subwoofer;
        s.denoise = self.denoise;
        s.gate = self.gate;
        s.eq = self.eq.clone();
        s.compressor = self.compressor;
        s.ducking = Ducking {
            buses: self.ducking.buses.iter().filter_map(bus).collect(),
            triggers: self
                .ducking
                .triggers
                .iter()
                .filter_map(|t| strip_ids.get(t).copied())
                .collect(),
            ..self.ducking.clone()
        };
    }
}

/// The mix of one bus, as a scene keeps it: the [`Bus`] settings that are
/// not about its device or its place in the mixer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BusMix {
    /// The bus's id when the scene was saved.
    pub id: BusId,
    /// The bus's name when the scene was saved, which is how a scene finds
    /// it again.
    pub name: String,
    /// See [`Bus::gain_db`].
    #[serde(default)]
    pub gain_db: f32,
    /// See [`Bus::mute`].
    #[serde(default)]
    pub mute: bool,
    /// See [`Bus::mono`].
    #[serde(default)]
    pub mono: bool,
    /// See [`Bus::eq`].
    #[serde(default, skip_serializing_if = "Equalizer::is_default")]
    pub eq: Equalizer,
    /// See [`Bus::limiter`].
    #[serde(default)]
    pub limiter: Limiter,
    /// See [`Bus::downmix`].
    #[serde(default, skip_serializing_if = "Downmix::is_default")]
    pub downmix: Downmix,
}

impl BusMix {
    fn of(b: &Bus) -> Self {
        Self {
            id: b.id,
            name: b.name.clone(),
            gain_db: b.gain_db,
            mute: b.mute,
            mono: b.mono,
            eq: b.eq.clone(),
            limiter: b.limiter,
            downmix: b.downmix,
        }
    }

    fn apply(&self, b: &mut Bus) {
        b.gain_db = self.gain_db;
        b.mute = self.mute;
        b.mono = self.mono;
        b.eq = self.eq.clone();
        b.limiter = self.limiter;
        b.downmix = self.downmix;
    }
}

/// A saved mix: levels, routes and effects, without the devices.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Scene {
    /// The mix of each strip.
    #[serde(default)]
    pub strips: Vec<StripMix>,
    /// The mix of each bus.
    #[serde(default)]
    pub buses: Vec<BusMix>,
}

/// How [`Scene::apply`] finds the strip or bus a saved mix belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchBy {
    /// By name first, then by id: for scenes, which should work on any
    /// setup with strips and buses of the same names.
    Name,
    /// By id only: for keeping the mix while switching setups, where the
    /// same strip keeps its id.
    Id,
}

impl MatchBy {
    /// Where in `items` the one saved as `id` and `name` is now.
    fn find<T>(
        self,
        items: &[T],
        id: u32,
        name: &str,
        key: impl Fn(&T) -> (u32, &str),
    ) -> Option<usize> {
        let by_id = || items.iter().position(|x| key(x).0 == id);
        match self {
            MatchBy::Id => by_id(),
            MatchBy::Name => items
                .iter()
                .position(|x| key(x).1.eq_ignore_ascii_case(name))
                .or_else(by_id),
        }
    }
}

impl Scene {
    /// The mix of `m`.
    pub fn capture(m: &MixerState) -> Self {
        Self {
            strips: m.strips.iter().map(StripMix::of).collect(),
            buses: m.buses.iter().map(BusMix::of).collect(),
        }
    }

    /// Put this mix onto the strips and buses of `m` it belongs to, leaving
    /// the rest alone. Routes and ducking are translated to the ids of the
    /// buses and strips they meant, and dropped where those are missing.
    pub fn apply(&self, m: &mut MixerState, by: MatchBy) {
        // Where each saved strip and bus is in `m` now, if anywhere.
        let strip_at: Vec<(&StripMix, usize)> = self
            .strips
            .iter()
            .filter_map(|s| {
                let i = by.find(&m.strips, s.id, &s.name, |x| (x.id, &x.name))?;
                Some((s, i))
            })
            .collect();
        let bus_at: Vec<(&BusMix, usize)> = self
            .buses
            .iter()
            .filter_map(|b| {
                let i = by.find(&m.buses, b.id, &b.name, |x| (x.id, &x.name))?;
                Some((b, i))
            })
            .collect();
        // Saved id -> id in `m`.
        let strip_ids: BTreeMap<StripId, StripId> = strip_at
            .iter()
            .map(|(s, i)| (s.id, m.strips[*i].id))
            .collect();
        let bus_ids: BTreeMap<BusId, BusId> =
            bus_at.iter().map(|(b, i)| (b.id, m.buses[*i].id)).collect();
        for (saved, i) in strip_at {
            saved.apply(&mut m.strips[i], &bus_ids, &strip_ids);
        }
        for (saved, i) in bus_at {
            saved.apply(&mut m.buses[i]);
        }
    }
}

/// Longest a scene's or setup's name may be.
pub const LIBRARY_NAME_MAX: usize = 64;

/// What is wrong with `name` as the name of a scene or setup, in words for
/// the person typing it, or `None` when it will do. Names become file names,
/// so they keep to letters, digits, spaces, `_`, `-` and `.`. Checked by the
/// daemon when saving, and by clients while the name is being typed.
pub fn library_name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("Type a name");
    }
    if name.len() > LIBRARY_NAME_MAX {
        return Some("Too long: 64 characters at most");
    }
    if name.starts_with('.') {
        return Some("A name cannot start with a dot");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.'))
    {
        return Some("Use only letters, numbers, spaces and - _ .");
    }
    None
}

/// The saved scenes and setups, and which of each was last loaded or saved.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Library {
    /// Every saved scene, by name, in alphabetical order.
    pub scenes: Vec<String>,
    /// Every saved setup, by name, in alphabetical order.
    pub setups: Vec<String>,
    /// The scene last loaded or saved, until it is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    /// The setup last loaded or saved, until it is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bus, BusKind, ChannelLayout, StripKind};

    fn mixer() -> MixerState {
        MixerState {
            strips: vec![
                Strip {
                    routes: [1].into_iter().collect(),
                    ..Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono)
                },
                Strip {
                    routes: [1, 2].into_iter().collect(),
                    ..Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo)
                },
            ],
            buses: vec![
                Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo),
                Bus::new(2, "Stream", BusKind::Virtual, ChannelLayout::Stereo),
            ],
        }
    }

    #[test]
    fn a_scene_brings_back_the_mix_and_leaves_devices_alone() {
        let mut m = mixer();
        m.strips[1].gain_db = -12.0;
        m.strips[1].sends.insert(2, -6.0);
        m.buses[0].mute = true;
        let scene = Scene::capture(&m);

        let mut now = mixer();
        now.buses[0].device = Some("alsa_output.usb".into());
        now.strips[1].name = "Music".into();
        scene.apply(&mut now, MatchBy::Name);
        assert_eq!(now.strips[1].gain_db, -12.0);
        assert_eq!(now.strips[1].send_db(2), -6.0);
        assert!(now.buses[0].mute);
        assert_eq!(now.buses[0].device.as_deref(), Some("alsa_output.usb"));
    }

    #[test]
    fn scenes_find_strips_by_name_and_translate_their_routes() {
        let mut m = mixer();
        m.strips[1].routes = [2].into_iter().collect();
        m.strips[1].ducking = Ducking {
            enabled: true,
            triggers: [1].into_iter().collect(),
            buses: [2].into_iter().collect(),
            ..Ducking::default()
        };
        let scene = Scene::capture(&m);

        // Another setup: the same names under other ids, and no Headset.
        let mut other = MixerState {
            strips: vec![
                Strip::new(7, "Music", StripKind::Virtual, ChannelLayout::Stereo),
                Strip::new(9, "Mic", StripKind::Hardware, ChannelLayout::Mono),
            ],
            buses: vec![Bus::new(
                5,
                "Stream",
                BusKind::Virtual,
                ChannelLayout::Stereo,
            )],
        };
        scene.apply(&mut other, MatchBy::Name);
        let music = &other.strips[0];
        assert_eq!(music.routes, [5].into_iter().collect());
        assert_eq!(music.ducking.triggers, [9].into_iter().collect());
        assert_eq!(music.ducking.buses, [5].into_iter().collect());
        // Mic was only routed to the Headset, which this setup lacks.
        assert!(other.strips[1].routes.is_empty());
    }

    #[test]
    fn library_names_are_checked_the_way_files_need() {
        assert_eq!(library_name_problem("Late night"), None);
        assert_eq!(library_name_problem("desk_2.v3-final"), None);
        assert!(library_name_problem("").is_some());
        assert!(library_name_problem("   ").is_some());
        assert!(library_name_problem(".hidden").is_some());
        assert!(library_name_problem("a/b").is_some());
        assert!(library_name_problem("café").is_some());
        assert!(library_name_problem(&"x".repeat(LIBRARY_NAME_MAX)).is_none());
        let too_long = library_name_problem(&"x".repeat(LIBRARY_NAME_MAX + 1));
        assert!(too_long.is_some_and(|p| p.contains(&LIBRARY_NAME_MAX.to_string())));
    }

    #[test]
    fn matching_by_id_ignores_names() {
        let mut m = mixer();
        m.strips[0].gain_db = -3.0;
        let scene = Scene::capture(&m);
        let mut renamed = mixer();
        renamed.strips[0].name = "Headset mic".into();
        scene.apply(&mut renamed, MatchBy::Id);
        assert_eq!(renamed.strips[0].gain_db, -3.0);
    }
}
