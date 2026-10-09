//! Preferences that belong to the window rather than to the mixer.
//!
//! Mixer state lives in the daemon so every client agrees on it. These are
//! purely about how this window draws things, so they stay local, in
//! `~/.config/weir/gui.toml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tracing::warn;

/// How much of a bus's incoming strip list to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusSources {
    /// Do not show it at all. The routing buttons already say the same thing.
    Hidden,
    /// Fit as many names as there is room for, with the rest in the tooltip.
    #[default]
    Compact,
    /// Show every name, scrolling when there are too many.
    Full,
}

impl BusSources {
    /// Every choice, in the order Preferences lists them.
    pub const ALL: [BusSources; 3] = [Self::Hidden, Self::Compact, Self::Full];

    /// Its name in Preferences.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hidden => "Hidden",
            Self::Compact => "Compact",
            Self::Full => "Full list",
        }
    }

    /// What it does, for its tooltip.
    pub fn description(self) -> &'static str {
        match self {
            Self::Hidden => "Nothing is shown. The routing buttons on the strips already say which bus each strip feeds.",
            Self::Compact => "Shows as many names as fit, and the rest on hover.",
            Self::Full => "Shows every name, scrolling if the list is long.",
        }
    }
}

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Whatever the desktop is set to, changing when it does.
    #[default]
    System,
    Dark,
    Light,
}

impl Appearance {
    /// Every choice, in the order Preferences lists them.
    pub const ALL: [Appearance; 3] = [Self::System, Self::Dark, Self::Light];

    /// Its name in Preferences.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Follow the desktop",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

/// What the equalizer's analyzer shows behind the curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpectrumView {
    Off,
    /// What comes out of the equalizer.
    Output,
    /// What goes in, faintly, and what comes out.
    #[default]
    InputAndOutput,
}

impl SpectrumView {
    /// Every choice, in the order the settings window lists them.
    pub const ALL: [SpectrumView; 3] = [Self::Off, Self::Output, Self::InputAndOutput];

    /// Its name above the equalizer curve.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Output => "Out",
            Self::InputAndOutput => "In + out",
        }
    }
}

/// Everything the window remembers for itself. A setting missing from the
/// file takes its default, and one the file has that is no longer used is
/// ignored, so the file never stops the window from starting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Light or dark.
    pub appearance: Appearance,
    /// Use the desktop's accent color in place of Weir's own teal.
    pub system_accent: bool,
    /// Draw text in the desktop's font rather than egui's.
    pub system_font: bool,
    /// How to display the list of strips feeding each bus.
    pub bus_sources: BusSources,
    /// Show a volume slider for each application under the virtual strips.
    pub show_app_volume: bool,
    /// The live spectrum behind the equalizer curve.
    pub spectrum: SpectrumView,
    /// Size of a strip's settings window, as last left.
    pub strip_fx_size: Option<[f32; 2]>,
    /// Size of a bus's settings window, as last left. A new name, so the
    /// smaller size the bus window had when it was an equalizer alone is
    /// not brought back.
    pub bus_settings_size: Option<[f32; 2]>,
    /// The friendly names of devices strips and buses have used, such as
    /// "Astro Mic", by their system names. PipeWire only describes the
    /// devices plugged in now, and without this an unplugged one could
    /// only be shown by a long name such as
    /// `alsa_input.usb-Astro_Gaming_Astro_MixAmp_Pro-00-input-0`.
    pub device_names: BTreeMap<String, String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            system_accent: false,
            system_font: false,
            bus_sources: BusSources::default(),
            show_app_volume: true,
            spectrum: SpectrumView::default(),
            strip_fx_size: None,
            bus_settings_size: None,
            device_names: BTreeMap::new(),
        }
    }
}

impl Prefs {
    /// Where the preferences are kept: `~/.config/weir/gui.toml`.
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("weir")
            .join("gui.toml")
    }

    /// The saved preferences, or the defaults when there are none or they
    /// cannot be read.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&text) {
            Ok(p) => p,
            Err(e) => {
                warn!("ignoring unreadable {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Take an imported window look, keeping everything else. A setting
    /// this window cannot read is passed over, keeping its own.
    pub fn take_look(&mut self, look: &serde_json::Value) {
        let serde_json::Value::Object(theirs) = look else {
            return;
        };
        for key in weir_protocol::WINDOW_LOOK_KEYS {
            let (Some(value), Ok(serde_json::Value::Object(mut mine))) =
                (theirs.get(*key), serde_json::to_value(&*self))
            else {
                continue;
            };
            mine.insert(key.to_string(), value.clone());
            if let Ok(taken) = serde_json::from_value(serde_json::Value::Object(mine)) {
                *self = taken;
            }
        }
    }

    /// Note the friendly names of the devices strips and buses use that are
    /// plugged in now. Returns whether any were new or changed, and so need
    /// saving.
    pub fn learn_device_names(&mut self, state: &weir_protocol::FullState) -> bool {
        let mixer = &state.mixer;
        let in_use = mixer.strips.iter().map(|s| &s.device);
        let in_use = in_use.chain(mixer.buses.iter().map(|b| &b.device));
        let mut changed = false;
        for name in in_use.flatten() {
            let Some(d) = state.devices.iter().find(|d| &d.name == name) else {
                continue;
            };
            if self.device_names.get(name) != Some(&d.description) {
                self.device_names
                    .insert(name.clone(), d.description.clone());
                changed = true;
            }
        }
        changed
    }

    /// Save the preferences. Failing to is logged, not fatal: the window
    /// works the same, and only forgets them when it closes.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                warn!("could not create {}: {e}", dir.display());
                return;
            }
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    warn!("could not save {}: {e}", path.display());
                }
            }
            Err(e) => warn!("could not serialize preferences: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_and_partial_files_still_load() {
        // A key that is no longer used, and most settings missing.
        let p: Prefs = toml::from_str("always_on_top = true\nbus_sources = \"full\"\n").unwrap();
        assert_eq!(p.bus_sources, BusSources::Full);
        assert!(p.show_app_volume, "missing settings take their defaults");
        let back: Prefs = toml::from_str(&toml::to_string_pretty(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn device_names_are_learned_for_devices_in_use() {
        use weir_protocol::*;
        let device = |name: &str, description: &str| DeviceInfo {
            id: 40,
            name: name.into(),
            description: description.into(),
            kind: DeviceKind::Source,
            channels: vec![ChannelPosition::Mono],
        };
        let mut state = FullState::default();
        state.mixer.strips.push(Strip {
            device: Some("alsa_input.usb-Astro".into()),
            ..Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono)
        });
        state.devices = vec![
            device("alsa_input.usb-Astro", "Astro Mic"),
            device("alsa_input.pci-0000", "Built-in Audio"),
        ];

        let mut p = Prefs::default();
        assert!(p.learn_device_names(&state));
        assert!(!p.learn_device_names(&state), "nothing new the second time");
        assert_eq!(p.device_names.len(), 1, "only devices in use are kept");
        assert_eq!(p.device_names["alsa_input.usb-Astro"], "Astro Mic");

        // Unplugged: the name is kept.
        state.devices.clear();
        assert!(!p.learn_device_names(&state));
        assert_eq!(p.device_names["alsa_input.usb-Astro"], "Astro Mic");
    }
}
