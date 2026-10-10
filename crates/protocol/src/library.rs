//! Scenes and setups: the two kinds of saved mixer state.
//!
//! A **setup** is what is there: which strips and buses there are, in
//! which order, their names, kinds, layouts, devices, colors and external
//! effects. Nothing about how they sound: loaded alone, every strip and
//! bus starts at its default mix, with nothing routed.
//!
//! A **scene** is how it sounds: levels, mutes, routes, route levels and
//! effects. It describes the whole mix: loading one gives each strip and
//! bus it has a mix for that mix, found by name, and every other one its
//! default mix, unrouted. It never changes which devices are used, so the
//! same "Streaming" or "Late night" scene works on any setup that has
//! strips and buses of the same names, and a setup can be loaded with one.

use crate::fx::{Compressor, Denoise, Ducking, Equalizer, Gate, Insert, Limiter};
use crate::model::{
    is_zero, Bus, BusId, BusKind, ChannelLayout, Downmix, MixerState, Strip, StripId, StripKind,
    Upmix,
};
use crate::targets::TargetKind;
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
    /// See [`Bus::delay_ms`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_ms: f32,
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
            delay_ms: b.delay_ms,
        }
    }

    fn apply(&self, b: &mut Bus) {
        b.gain_db = self.gain_db;
        b.mute = self.mute;
        b.mono = self.mono;
        b.eq = self.eq.clone();
        b.limiter = self.limiter;
        b.downmix = self.downmix;
        b.delay_ms = self.delay_ms;
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

impl Scene {
    /// The mix of `m`.
    pub fn capture(m: &MixerState) -> Self {
        Self {
            strips: m.strips.iter().map(StripMix::of).collect(),
            buses: m.buses.iter().map(BusMix::of).collect(),
        }
    }

    /// Put this mix onto `m`. Each strip and bus this scene has a mix for,
    /// found by name, takes it; every other one goes back to its default
    /// mix, unrouted, since a scene describes the whole mix. Routes and
    /// ducking are translated to the ids the buses and strips they meant
    /// have in `m`, and dropped where those are missing.
    pub fn apply(&self, m: &mut MixerState) {
        let named = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        let strip_mix: Vec<Option<&StripMix>> = m
            .strips
            .iter()
            .map(|x| self.strips.iter().find(|s| named(&s.name, &x.name)))
            .collect();
        let bus_mix: Vec<Option<&BusMix>> = m
            .buses
            .iter()
            .map(|x| self.buses.iter().find(|b| named(&b.name, &x.name)))
            .collect();
        // Saved id -> id in `m`.
        let strip_ids: BTreeMap<StripId, StripId> = strip_mix
            .iter()
            .zip(&m.strips)
            .filter_map(|(saved, x)| Some((saved.as_ref()?.id, x.id)))
            .collect();
        let bus_ids: BTreeMap<BusId, BusId> = bus_mix
            .iter()
            .zip(&m.buses)
            .filter_map(|(saved, x)| Some((saved.as_ref()?.id, x.id)))
            .collect();
        for (s, saved) in m.strips.iter_mut().zip(strip_mix) {
            match saved {
                Some(saved) => saved.apply(s, &bus_ids, &strip_ids),
                None => StripMix::of(&Strip::new(s.id, "", s.kind, s.layout.clone())).apply(
                    s,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                ),
            }
        }
        for (b, saved) in m.buses.iter_mut().zip(bus_mix) {
            match saved {
                Some(saved) => saved.apply(b),
                None => BusMix::of(&Bus::new(b.id, "", b.kind, b.layout.clone())).apply(b),
            }
        }
    }

    /// Who this scene has a mix for, by name.
    pub fn members(&self) -> Members {
        Members {
            strips: self.strips.iter().map(|s| s.name.clone()).collect(),
            buses: self.buses.iter().map(|b| b.name.clone()).collect(),
        }
    }
}

/// What a setup keeps of one strip: what makes it the strip it is, and
/// nothing about how it sounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetupStrip {
    /// See [`Strip::id`].
    pub id: StripId,
    /// See [`Strip::name`].
    pub name: String,
    /// See [`Strip::kind`].
    pub kind: StripKind,
    /// See [`Strip::layout`].
    #[serde(default)]
    pub layout: ChannelLayout,
    /// See [`Strip::device`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// See [`Strip::color`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// See [`Strip::insert`]: switching external effects on makes devices
    /// another program is wired to, so they belong with the devices.
    #[serde(default, skip_serializing_if = "Insert::is_default")]
    pub insert: Insert,
}

/// What a setup keeps of one bus: what makes it the bus it is, and nothing
/// about how it sounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetupBus {
    /// See [`Bus::id`].
    pub id: BusId,
    /// See [`Bus::name`].
    pub name: String,
    /// See [`Bus::kind`].
    pub kind: BusKind,
    /// See [`Bus::layout`].
    #[serde(default)]
    pub layout: ChannelLayout,
    /// See [`Bus::device`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// See [`Bus::color`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// See [`Bus::insert`].
    #[serde(default, skip_serializing_if = "Insert::is_default")]
    pub insert: Insert,
}

/// A saved setup: the strips and buses there are, in order, and their
/// devices, without the mix. Files saved before setups and scenes were
/// told apart hold a whole mixer; they read as this, the mix passed over.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Setup {
    /// Every strip, left to right.
    #[serde(default)]
    pub strips: Vec<SetupStrip>,
    /// Every bus, left to right.
    #[serde(default)]
    pub buses: Vec<SetupBus>,
}

impl Setup {
    /// What is there in `m`.
    pub fn capture(m: &MixerState) -> Self {
        Self {
            strips: m
                .strips
                .iter()
                .map(|s| SetupStrip {
                    id: s.id,
                    name: s.name.clone(),
                    kind: s.kind,
                    layout: s.layout.clone(),
                    device: s.device.clone(),
                    color: s.color.clone(),
                    insert: s.insert,
                })
                .collect(),
            buses: m
                .buses
                .iter()
                .map(|b| SetupBus {
                    id: b.id,
                    name: b.name.clone(),
                    kind: b.kind,
                    layout: b.layout.clone(),
                    device: b.device.clone(),
                    color: b.color.clone(),
                    insert: b.insert,
                })
                .collect(),
        }
    }

    /// The mixer this setup makes, every strip and bus at its default mix
    /// with nothing routed: what loading it with no scene gives.
    pub fn mixer(&self) -> MixerState {
        MixerState {
            strips: self
                .strips
                .iter()
                .map(|s| Strip {
                    device: s.device.clone(),
                    color: s.color.clone(),
                    insert: s.insert,
                    ..Strip::new(s.id, s.name.clone(), s.kind, s.layout.clone())
                })
                .collect(),
            buses: self
                .buses
                .iter()
                .map(|b| Bus {
                    device: b.device.clone(),
                    color: b.color.clone(),
                    insert: b.insert,
                    ..Bus::new(b.id, b.name.clone(), b.kind, b.layout.clone())
                })
                .collect(),
        }
    }

    /// Its strips and buses, by name.
    pub fn members(&self) -> Members {
        Members {
            strips: self.strips.iter().map(|s| s.name.clone()).collect(),
            buses: self.buses.iter().map(|b| b.name.clone()).collect(),
        }
    }
}

/// The strips and buses of a saved scene or setup, by name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Members {
    /// Strips, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strips: Vec<String>,
    /// Buses, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buses: Vec<String>,
}

impl Members {
    /// The strips and buses of `m`, by name.
    pub fn of_mixer(m: &MixerState) -> Self {
        Self {
            strips: m.strips.iter().map(|s| s.name.clone()).collect(),
            buses: m.buses.iter().map(|b| b.name.clone()).collect(),
        }
    }

    /// Those of these that `there` has none called: what a scene with these
    /// members would find missing in a setup with `there`'s.
    pub fn missing_in(&self, there: &Members) -> Vec<(TargetKind, String)> {
        let lacks = |names: &[String], name: &String| {
            !names
                .iter()
                .any(|n| n.trim().eq_ignore_ascii_case(name.trim()))
        };
        let strips = self
            .strips
            .iter()
            .filter(|n| lacks(&there.strips, n))
            .map(|n| (TargetKind::Strip, n.clone()));
        let buses = self
            .buses
            .iter()
            .filter(|n| lacks(&there.buses, n))
            .map(|n| (TargetKind::Bus, n.clone()));
        strips.chain(buses).collect()
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
    /// Who each saved scene has a mix for, by the scene's name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scene_members: BTreeMap<String, Members>,
    /// The strips and buses of each saved setup, by the setup's name. With
    /// `scene_members`, which strips and buses a scene would find missing
    /// in a setup.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub setup_members: BTreeMap<String, Members>,
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
        scene.apply(&mut now);
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
        scene.apply(&mut other);
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
    fn what_a_scene_does_not_mention_goes_back_to_default() {
        let mut m = mixer();
        m.strips[0].gain_db = -3.0;
        m.buses[1].delay_ms = 40.0;
        let scene = Scene::capture(&m);

        // Mic renamed: the scene has no mix for "Headset mic", and the same
        // id is not taken for the same strip. A strip the scene never knew
        // starts at its default, unrouted.
        let mut now = mixer();
        now.strips[0].name = "Headset mic".into();
        now.strips[0].gain_db = -9.0;
        now.strips.push(Strip {
            gain_db: -20.0,
            routes: [1].into_iter().collect(),
            ..Strip::new(3, "Game", StripKind::Virtual, ChannelLayout::Stereo)
        });
        now.buses[0].limiter = Limiter::on();
        let mut headset = Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo);
        headset.limiter = Limiter::on();
        now.buses[0] = headset;
        scene.apply(&mut now);
        assert_eq!(now.strips[0].gain_db, 0.0);
        assert!(now.strips[0].routes.is_empty());
        assert_eq!(now.strips[2].gain_db, 0.0);
        assert!(now.strips[2].routes.is_empty());
        assert_eq!(now.strips[1].routes, [1, 2].into_iter().collect());
        // The bus delay is part of the mix now; the Headset's limiter is
        // the scene's (off), not what it had.
        assert_eq!(now.buses[1].delay_ms, 40.0);
        assert!(!now.buses[0].limiter.enabled);
    }

    #[test]
    fn a_setup_keeps_what_is_there_and_nothing_of_the_mix() {
        let mut m = mixer();
        m.strips[0].device = Some("alsa_input.usb".into());
        m.strips[0].color = Some("#ff8800".into());
        m.strips[1].gain_db = -12.0;
        m.buses[1].limiter.enabled = false;
        let setup = Setup::capture(&m);
        let back = setup.mixer();
        assert_eq!(back.strips[0].device.as_deref(), Some("alsa_input.usb"));
        assert_eq!(back.strips[0].color.as_deref(), Some("#ff8800"));
        assert_eq!(back.strips[1].gain_db, 0.0);
        assert!(back.strips.iter().all(|s| s.routes.is_empty()));
        // A virtual bus starts with its limiter on, as a new one does.
        assert!(back.buses[1].limiter.enabled);

        // A setup saved as a whole mixer, before the two were told apart,
        // reads as a setup.
        let old: Setup = serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(old, setup);
    }

    #[test]
    fn missing_members_are_named() {
        let scene = Members {
            strips: vec!["Mic".into(), "Podcast".into()],
            buses: vec!["Stream".into()],
        };
        let there = Members::of_mixer(&mixer());
        assert_eq!(
            scene.missing_in(&there),
            vec![(TargetKind::Strip, "Podcast".to_string())]
        );
    }
}
