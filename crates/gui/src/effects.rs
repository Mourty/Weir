//! External effects as the window shows them: whether a strip's or bus's are
//! connected, what that means for its sound, and the badge that says so at
//! a glance beside its name.

use crate::theme;
use egui::{vec2, Color32, Response, RichText, Ui};
use weir_protocol::{FullState, Insert, InsertFallback, InsertPatch, Request, StripOrBus};

/// A strip's or bus's external effects, and whether anything plays into
/// its "back from effects" device.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub insert: Insert,
    pub connected: bool,
}

impl Status {
    /// `target`'s, while it has them on.
    pub fn of(state: &FullState, target: StripOrBus) -> Option<Self> {
        let insert = match target {
            StripOrBus::Strip(id) => state.mixer.strip(id)?.insert,
            StripOrBus::Bus(id) => state.mixer.bus(id)?.insert,
        };
        insert.enabled.then(|| Self {
            insert,
            connected: state
                .inserts
                .iter()
                .any(|i| i.target == target && i.connected),
        })
    }

    /// The color of their badge and chain stage: their own while connected,
    /// the warning yellow while not.
    pub fn color(&self) -> Color32 {
        if self.connected {
            theme::p().ext_on
        } else {
            theme::p().meter_yellow
        }
    }

    /// What they are doing to the sound right now, in a few words.
    pub fn short(&self) -> &'static str {
        match (self.connected, self.insert.fallback) {
            (true, _) => "connected",
            (false, InsertFallback::PassThrough) => "not connected, passing through",
            (false, InsertFallback::Silence) => "not connected, silent",
        }
    }

    /// The same at more length, for `name` and its devices.
    pub fn explain(&self, name: &str) -> String {
        let (to, back) = device_names(name);
        match (self.connected, self.insert.fallback) {
            (true, _) => format!(
                "External effects are connected: {name}'s sound goes out through \"{to}\" \
                 and comes back through \"{back}\"."
            ),
            (false, InsertFallback::PassThrough) => format!(
                "External effects are not connected: nothing plays into \"{back}\", so \
                 {name} carries on as if they were off, without effects."
            ),
            (false, InsertFallback::Silence) => format!(
                "External effects are not connected: nothing plays into \"{back}\", so \
                 {name} is silent until something does."
            ),
        }
    }
}

/// The two devices the external effects of a strip or bus called `name`
/// use: the one the effects program records from, and the one it plays
/// into.
pub fn device_names(name: &str) -> (String, String) {
    (
        format!("{name}: to effects (Weir)"),
        format!("{name}: back from effects (Weir)"),
    )
}

/// The name people know the device called `node` by, when it is one an
/// effects program plays back into, such as "Music: back from effects
/// (Weir)": an effects program's output shows among the applications.
/// The engine names the device `weir.from-effects.strip.2`.
pub fn return_device(state: &FullState, node: &str) -> Option<String> {
    let (kind, id) = node.strip_prefix("weir.from-effects.")?.split_once('.')?;
    let id = id.parse().ok()?;
    let name = match kind {
        "strip" => &state.mixer.strip(id)?.name,
        "bus" => &state.mixer.bus(id)?.name,
        _ => return None,
    };
    Some(device_names(name).1)
}

/// The request that changes `target`'s external effects as `patch` says.
pub fn request(target: StripOrBus, patch: InsertPatch) -> Request {
    match target {
        StripOrBus::Strip(id) => Request::SetStrip(weir_protocol::StripPatch {
            id,
            insert: Some(patch),
            ..Default::default()
        }),
        StripOrBus::Bus(id) => Request::SetBus(weir_protocol::BusPatch {
            id,
            insert: Some(patch),
            ..Default::default()
        }),
    }
}

/// Width of the badge beside a name.
pub const BADGE_W: f32 = 34.0;

/// The badge beside the name of a strip or bus called `name` with external
/// effects on: "EXT" in their color while connected, "EXT !" in yellow while
/// not. Clicked, it should open their settings.
pub fn badge(ui: &mut Ui, status: &Status, name: &str, height: f32) -> Response {
    let label = if status.connected { "EXT" } else { "EXT !" };
    let text = RichText::new(label)
        .size(10.0)
        .strong()
        .color(theme::p().lamp_on_text);
    ui.add(
        egui::Button::new(text)
            .fill(status.color())
            .min_size(vec2(BADGE_W, height))
            .corner_radius(4),
    )
    .on_hover_text(format!(
        "{}\n\nClick for their settings.",
        status.explain(name)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use weir_protocol::*;

    fn state(enabled: bool, connected: bool) -> FullState {
        let mut mic = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
        mic.insert.enabled = enabled;
        FullState {
            mixer: MixerState {
                strips: vec![mic],
                buses: Vec::new(),
            },
            inserts: vec![InsertStatus {
                target: StripOrBus::Strip(1),
                connected,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn only_effects_that_are_on_have_a_status() {
        let target = StripOrBus::Strip(1);
        assert!(Status::of(&state(false, true), target).is_none());
        let on = Status::of(&state(true, true), target).unwrap();
        assert!(on.connected);
        let waiting = Status::of(&state(true, false), target).unwrap();
        assert_eq!(waiting.short(), "not connected, passing through");
        assert!(waiting
            .explain("Mic")
            .contains("Mic: back from effects (Weir)"));
    }

    #[test]
    fn an_effects_program_plays_into_a_device_people_know_by_name() {
        let st = state(true, true);
        assert_eq!(
            return_device(&st, "weir.from-effects.strip.1").as_deref(),
            Some("Mic: back from effects (Weir)")
        );
        assert_eq!(return_device(&st, "weir.from-effects.strip.9"), None);
        assert_eq!(return_device(&st, "weir.input.1"), None);
    }
}
