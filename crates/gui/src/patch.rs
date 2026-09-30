//! Changes that strips and buses have in common, for the controls that work
//! the same on either: renaming, recoloring, the fader, mute, the device,
//! the channel layout and the equalizer.

use weir_protocol::{BusPatch, ChannelLayout, EqPatch, Flag, Request, StripOrBus, StripPatch};

/// The fields a strip's patch and a bus's patch share. [`CommonPatch::to`]
/// turns it into a `set_strip` or `set_bus` request.
#[derive(Debug, Clone, Default)]
pub struct CommonPatch {
    pub name: Option<String>,
    pub gain_db: Option<f32>,
    pub mute: Option<Flag>,
    pub layout: Option<ChannelLayout>,
    /// `Some(None)` clears it.
    pub device: Option<Option<String>>,
    /// `Some(None)` clears it.
    pub color: Option<Option<String>>,
    pub eq: Option<EqPatch>,
}

impl CommonPatch {
    /// The request that makes this change to `target`.
    pub fn to(self, target: StripOrBus) -> Request {
        match target {
            StripOrBus::Strip(id) => Request::SetStrip(StripPatch {
                id,
                name: self.name,
                gain_db: self.gain_db,
                mute: self.mute,
                layout: self.layout,
                device: self.device,
                color: self.color,
                eq: self.eq,
                ..Default::default()
            }),
            StripOrBus::Bus(id) => Request::SetBus(BusPatch {
                id,
                name: self.name,
                gain_db: self.gain_db,
                mute: self.mute,
                layout: self.layout,
                device: self.device,
                color: self.color,
                eq: self.eq,
                ..Default::default()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_change_goes_to_a_strip_or_a_bus() {
        let mute = || CommonPatch {
            mute: Some(Flag::Set(true)),
            ..Default::default()
        };
        let Request::SetStrip(s) = mute().to(StripOrBus::Strip(3)) else {
            panic!("expected set_strip");
        };
        assert_eq!((s.id, s.mute), (3, Some(Flag::Set(true))));
        let Request::SetBus(b) = mute().to(StripOrBus::Bus(2)) else {
            panic!("expected set_bus");
        };
        assert_eq!((b.id, b.mute, b.name), (2, Some(Flag::Set(true)), None));
    }
}
