//! The colors meters are drawn in: colors from the bottom of the meter up,
//! and the levels where each gives way to the next. Weir's own scheme,
//! Default, is green, yellow from -12 dB and red from 0 dB. People make
//! their own, with any number of bands up to [`METER_BANDS_MAX`], and keep
//! them by name, as the daemon keeps equalizer presets. Which one a
//! window draws with is the window's own choice.

use crate::model::{GAIN_MAX_DB, GAIN_MIN_DB};
use serde::{Deserialize, Serialize};

/// The name of Weir's own scheme, which cannot be changed or removed.
pub const METER_COLORS_DEFAULT: &str = "Default";
/// Most bands a scheme may have.
pub const METER_BANDS_MAX: usize = 12;
/// The closest two lines between bands may be, in dB.
pub const METER_BREAK_GAP_DB: f32 = 1.0;

fn is_false(b: &bool) -> bool {
    !*b
}

/// A way of coloring meters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MeterColors {
    /// Its name, 1 to 40 characters.
    pub name: String,
    /// The colors from the bottom of the meter up, as `#RRGGBB`: one more
    /// than there are `breaks_db`.
    pub colors: Vec<String>,
    /// Where each color gives way to the next, in dB, rising, from -60 to
    /// +12, at least 1 dB apart. A level at a line takes the color above
    /// it.
    #[serde(default)]
    pub breaks_db: Vec<f32>,
    /// Comes with Weir: it cannot be changed or removed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub builtin: bool,
}

impl MeterColors {
    /// Weir's own scheme. A window may draw it in its theme's shades of
    /// the same green, yellow and red.
    pub fn weir_default() -> Self {
        Self {
            name: METER_COLORS_DEFAULT.into(),
            colors: vec!["#46C864".into(), "#EBC846".into(), "#EB4B41".into()],
            breaks_db: vec![-12.0, 0.0],
            builtin: true,
        }
    }

    /// Which band, counting from the bottom, a level falls in.
    pub fn band_at(&self, db: f32) -> usize {
        self.breaks_db.iter().take_while(|b| db >= **b).count()
    }

    /// What is wrong with it as a scheme to keep, if anything.
    pub fn problem(&self) -> Option<String> {
        let n = self.colors.len();
        if n == 0 {
            return Some("it has no colors".into());
        }
        if n > METER_BANDS_MAX {
            return Some(format!("it has more than {METER_BANDS_MAX} colors"));
        }
        if self.breaks_db.len() + 1 != n {
            return Some(format!(
                "{n} colors need {} lines between them, not {}",
                n - 1,
                self.breaks_db.len()
            ));
        }
        if let Some(bad) = self.colors.iter().find(|c| parse_hex_color(c).is_none()) {
            return Some(format!("'{bad}' is not a color such as #46C864"));
        }
        if let Some(b) = self
            .breaks_db
            .iter()
            .find(|b| !b.is_finite() || **b < GAIN_MIN_DB || **b > GAIN_MAX_DB)
        {
            return Some(format!(
                "a line at {b} dB is off the meter, which reads {GAIN_MIN_DB} to +{GAIN_MAX_DB} dB"
            ));
        }
        if self
            .breaks_db
            .windows(2)
            .any(|w| w[1] - w[0] < METER_BREAK_GAP_DB - 1e-3)
        {
            return Some(format!(
                "its lines must rise, at least {METER_BREAK_GAP_DB} dB apart"
            ));
        }
        None
    }

    /// Its colors as red, green and blue; gray for one that cannot be
    /// read.
    pub fn rgb(&self) -> Vec<[u8; 3]> {
        self.colors
            .iter()
            .map(|c| parse_hex_color(c).unwrap_or([128, 128, 128]))
            .collect()
    }
}

/// A color written `#RRGGBB` (or `RRGGBB`) as red, green and blue.
pub fn parse_hex_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// A color as `#RRGGBB`.
pub fn hex_color(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_fall_in_bands_and_schemes_are_checked() {
        let d = MeterColors::weir_default();
        assert_eq!(d.problem(), None);
        assert_eq!(
            (
                d.band_at(-30.0),
                d.band_at(-12.0),
                d.band_at(-0.1),
                d.band_at(3.0)
            ),
            (0, 1, 1, 2)
        );
        let mut s = d.clone();
        s.breaks_db = vec![0.0, -12.0];
        assert!(s.problem().unwrap().contains("rise"));
        s.breaks_db = vec![-12.0];
        assert!(s.problem().unwrap().contains("need 2 lines"));
        s.breaks_db = vec![-12.0, 20.0];
        assert!(s.problem().unwrap().contains("off the meter"));
        s.breaks_db = vec![-12.0, 0.0];
        s.colors[1] = "yellow".into();
        assert!(s.problem().unwrap().contains("not a color"));
        assert_eq!(parse_hex_color("#eb4b41"), Some([235, 75, 65]));
        assert_eq!(parse_hex_color("EB4B41"), Some([235, 75, 65]));
        assert_eq!(parse_hex_color("#EB4B4"), None);
        assert_eq!(hex_color([70, 200, 100]), "#46C864");
    }
}
