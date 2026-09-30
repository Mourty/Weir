//! Reading and writing an application stream's own volume.
//!
//! PipeWire keeps a per-stream volume on the node itself, as the
//! `channelVolumes` and `mute` properties of its `Props` parameter. Those are
//! the same values `pavucontrol` and the KDE volume applet manipulate, so a
//! change made here shows up there and vice versa.
//!
//! Volumes are stored as linear amplitude, one per channel, where 1.0 is
//! unity gain. The rest of Weir works in decibels, so the daemon
//! converts at the edges.

use libspa::pod::deserialize::PodDeserializer;
use libspa::pod::serialize::PodSerializer;
use libspa::pod::{Object, Property, PropertyFlags, Value, ValueArray};
use libspa_sys as spa_sys;
use std::io::Cursor;

/// A stream's own volume, as PipeWire stores it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppVolume {
    /// Linear gain per channel. Empty when the stream has not reported one.
    pub channel_volumes: Vec<f32>,
    /// Whether the stream itself is muted.
    pub mute: bool,
}

impl AppVolume {
    /// The level to show for the stream: the loudest channel, linear.
    pub fn level(&self) -> Option<f32> {
        self.channel_volumes.iter().copied().reduce(f32::max)
    }
}

/// Build a `Props` pod setting the given per-channel linear gains and,
/// optionally, the mute flag. Returns `None` when there is nothing to set.
pub fn build_props(volumes: &[f32], mute: Option<bool>) -> Option<Vec<u8>> {
    let mut properties = Vec::new();
    if !volumes.is_empty() {
        properties.push(Property {
            key: spa_sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(volumes.to_vec())),
        });
    }
    if let Some(mute) = mute {
        properties.push(Property {
            key: spa_sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(mute),
        });
    }
    if properties.is_empty() {
        return None;
    }
    let object = Value::Object(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Props,
        id: spa_sys::SPA_PARAM_Props,
        properties,
    });
    let (cursor, _) = PodSerializer::serialize(Cursor::new(Vec::new()), &object).ok()?;
    Some(cursor.into_inner())
}

/// Extract the volume and mute state from a `Props` pod reported by a node.
///
/// Properties we do not care about are ignored, and a pod that carries
/// neither volume nor mute yields `None`.
pub fn parse_props(bytes: &[u8]) -> Option<AppVolume> {
    let (_, value) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let Value::Object(object) = value else {
        return None;
    };
    if object.type_ != spa_sys::SPA_TYPE_OBJECT_Props {
        return None;
    }
    let mut out = AppVolume::default();
    let mut found = false;
    for property in &object.properties {
        match (property.key, &property.value) {
            (spa_sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(v))) => {
                out.channel_volumes = v.clone();
                found = true;
            }
            (spa_sys::SPA_PROP_mute, Value::Bool(m)) => {
                out.mute = *m;
                found = true;
            }
            _ => {}
        }
    }
    found.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libspa::pod::Pod;

    #[test]
    fn round_trip_volume_and_mute() {
        let bytes = build_props(&[0.5, 0.25], Some(true)).expect("builds");
        let parsed = parse_props(&bytes).expect("parses");
        assert_eq!(parsed.channel_volumes, vec![0.5, 0.25]);
        assert!(parsed.mute);
        assert_eq!(parsed.level(), Some(0.5));
    }

    #[test]
    fn round_trip_volume_only() {
        let bytes = build_props(&[1.0, 1.0, 1.0], None).expect("builds");
        let parsed = parse_props(&bytes).expect("parses");
        assert_eq!(parsed.channel_volumes, vec![1.0; 3]);
        assert!(!parsed.mute);
    }

    #[test]
    fn mute_only_leaves_volumes_empty() {
        let bytes = build_props(&[], Some(true)).expect("builds");
        let parsed = parse_props(&bytes).expect("parses");
        assert!(parsed.channel_volumes.is_empty());
        assert!(parsed.mute);
        assert_eq!(parsed.level(), None);
    }

    #[test]
    fn nothing_to_set_is_none() {
        assert!(build_props(&[], None).is_none());
    }

    #[test]
    fn rejects_unrelated_pods() {
        // A plain value is not a Props object.
        let (cursor, _) =
            PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Int(7)).expect("serializes");
        assert!(parse_props(&cursor.into_inner()).is_none());
    }

    #[test]
    fn pod_bytes_are_accepted_by_the_pod_wrapper() {
        // What we build must be a pod PipeWire will accept.
        let bytes = build_props(&[0.75], Some(false)).expect("builds");
        assert!(Pod::from_bytes(&bytes).is_some());
    }
}
