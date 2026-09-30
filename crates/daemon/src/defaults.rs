//! The layout written on first run: a mono mic, three virtual inputs, a
//! hardware headset bus, a spare hardware bus and a virtual "stream mic".

use weir_protocol::*;

/// The mixer a first run starts with. Its hardware strip and buses have no
/// devices yet; the user picks them.
pub fn default_mixer() -> MixerState {
    let strip = |id, name: &str, kind, layout, routes: &[BusId]| Strip {
        routes: routes.iter().copied().collect(),
        ..Strip::new(id, name, kind, layout)
    };
    let bus = Bus::new;
    MixerState {
        strips: vec![
            strip(1, "Mic", StripKind::Hardware, ChannelLayout::Mono, &[3]),
            strip(
                2,
                "Music",
                StripKind::Virtual,
                ChannelLayout::Stereo,
                &[1, 3],
            ),
            strip(
                3,
                "Browser",
                StripKind::Virtual,
                ChannelLayout::Stereo,
                &[1, 3],
            ),
            strip(
                4,
                "Soundboard",
                StripKind::Virtual,
                ChannelLayout::Stereo,
                &[1, 3],
            ),
        ],
        buses: vec![
            bus(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo),
            bus(2, "Speakers", BusKind::Hardware, ChannelLayout::Stereo),
            bus(3, "Stream Mic", BusKind::Virtual, ChannelLayout::Stereo),
        ],
    }
}
