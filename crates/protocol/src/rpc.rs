//! The control protocol: JSON-RPC 2.0 envelopes, every method with its
//! parameters, and every notification.
//!
//! Wire format: one JSON object per line (`\n` terminated), UTF-8, over a
//! Unix domain socket. Requests carry `jsonrpc`, `id`, `method` and optional
//! `params`; responses carry `id` and either `result` or `error`; server
//! notifications carry `method` and `params` but no `id`. A line may also
//! hold a batch: an array of requests, answered by an array of responses.
//!
//! Changes are partial: a `*Patch` names only what it changes, and anything
//! it leaves out stays as it is. On/off settings take a [`Flag`], which can
//! also say "toggle", and levels usually have a `*_delta_*` twin that moves
//! them by an amount, so a button or a knob needs no idea of the current
//! state to do its job.

use crate::fx::*;
use crate::hotkeys::*;
use crate::library::Library;
use crate::model::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The `jsonrpc` member of every message.
pub const JSONRPC_VERSION: &str = "2.0";

/// Raw request envelope as it appears on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RpcRequest {
    /// Always `"2.0"`; may be left out.
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// Any JSON value, echoed in the response. A request without one is a
    /// notification in JSON-RPC terms, and gets no response at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// The method's name, such as `set_strip`.
    pub method: String,
    /// The method's parameters, if it takes any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

fn jsonrpc_version() -> String {
    JSONRPC_VERSION.to_string()
}

impl RpcRequest {
    /// An envelope for `request`, with `id`.
    pub fn new(id: impl Into<Value>, request: &Request) -> Self {
        let v = serde_json::to_value(request).expect("request serializes");
        let method = v["method"].as_str().unwrap_or_default().to_string();
        let params = v.get("params").cloned();
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(id.into()),
            method,
            params,
        }
    }

    /// Convert the raw envelope into a typed [`Request`].
    ///
    /// A method whose parameters are all optional may be called without
    /// `params` at all, as if it were given `{}`; and a method without
    /// parameters may be given an empty `{}` or `[]`, as many JSON-RPC
    /// libraries always send one.
    pub fn parse(&self) -> Result<Request, RpcError> {
        let with = |params: Option<&Value>| {
            serde_json::from_value::<Request>(
                serde_json::json!({ "method": self.method, "params": params }),
            )
        };
        // Serde reports a method it does not know as an unknown variant of
        // `Request`. So would it a value it does not know inside the
        // parameters, such as a misspelled layout, so ask about the method
        // with no parameters to trip over.
        let known = !with(None)
            .as_ref()
            .is_err_and(|e| e.to_string().contains("unknown variant"));
        if !known {
            return Err(RpcError::method_not_found(&self.method));
        }
        let empty = match &self.params {
            Some(Value::Object(m)) => m.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            _ => false,
        };
        // What to try if the parameters as given do not fit: `{}` for none
        // at all, and none at all for empty ones.
        let instead = match &self.params {
            None => Some(serde_json::json!({})),
            Some(_) if empty => Some(Value::Null),
            Some(_) => None,
        };
        let result = match (with(self.params.as_ref()), instead) {
            (Err(e), Some(params)) => with(Some(&params)).map_err(|_| e),
            (result, _) => result,
        };
        result.map_err(|e| RpcError::invalid_params(e.to_string()))
    }
}

/// Raw response envelope as it appears on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RpcResponse {
    /// Always `"2.0"`.
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// The `id` of the request this answers; `null` when the request could
    /// not be read far enough to find one.
    pub id: Value,
    /// What the method returned, when it worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Why it did not, when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// A response carrying `result`.
    pub fn ok(id: Value, result: impl Serialize) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: Some(serde_json::to_value(result).expect("result serializes")),
            error: None,
        }
    }

    /// A response carrying `error`.
    pub fn err(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// An error in a response. The message is written for people and may be
/// shown to them as it is.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RpcError {
    /// One of the codes below.
    pub code: i32,
    /// What went wrong, in words.
    pub message: String,
    /// More detail, when there is any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::error::Error for RpcError {}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl RpcError {
    /// The line was not JSON.
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON was not a request, or was an empty batch.
    pub const INVALID_REQUEST: i32 = -32600;
    /// There is no method by that name.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// The parameters are missing something, have something of the wrong
    /// type, or have a value out of range.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Something went wrong inside the daemon.
    pub const INTERNAL: i32 = -32603;
    /// Request was well formed but refers to something that does not exist
    /// or violates a mixer rule (unknown strip id, duplicate name, ...).
    pub const APPLICATION: i32 = -32000;
    /// The audio engine reported a failure.
    pub const ENGINE: i32 = -32001;

    /// An error with `code` and `message`.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// A [`Self::PARSE_ERROR`].
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self::new(Self::PARSE_ERROR, msg)
    }

    /// An [`Self::INVALID_REQUEST`].
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(Self::INVALID_REQUEST, msg)
    }

    /// A [`Self::METHOD_NOT_FOUND`] for `method`.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(Self::METHOD_NOT_FOUND, format!("unknown method '{method}'"))
    }

    /// An [`Self::INVALID_PARAMS`].
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(Self::INVALID_PARAMS, msg)
    }

    /// An [`Self::APPLICATION`] error.
    pub fn application(msg: impl Into<String>) -> Self {
        Self::new(Self::APPLICATION, msg)
    }

    /// An [`Self::ENGINE`] error.
    pub fn engine(msg: impl Into<String>) -> Self {
        Self::new(Self::ENGINE, msg)
    }

    /// An [`Self::INTERNAL`] error.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(Self::INTERNAL, msg)
    }
}

/// Notification topics a client can subscribe to.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// `state_changed`, `eq_presets_changed`, `history_changed`,
    /// `app_rules_changed` and `library_changed`: the mixer, the equalizer
    /// presets, what can be undone, the application rules, or the saved
    /// scenes and setups changed.
    State,
    /// `meters`: periodic peak levels.
    Meters,
    /// `devices_changed`, `system_volumes_changed` and `inserts_changed`: a
    /// device appeared or vanished, the system volume of one of Weir's
    /// devices changed, or external effects were connected or disconnected.
    Devices,
    /// `apps_changed`: an application stream appeared, vanished or moved.
    Apps,
    /// `engine_changed`: engine connection state, sample rate or quantum.
    Engine,
    /// `show_window` and `quit`: the daemon asking the mixer window to come
    /// to the front, or to close. Subscribing marks this connection as a
    /// window, which is how the tray knows whether one is already open.
    Window,
    /// `settings_changed`: the daemon's own settings.
    Settings,
    /// `spectrum`: sent for the strips and buses a connection asked for with
    /// `watch_spectrum`, whatever it subscribed to.
    Spectrum,
    /// `hotkeys_changed`: the hotkeys, or how keys reach Weir.
    Hotkeys,
}

impl Topic {
    /// Everything a general-purpose client wants. `Window` is deliberately
    /// excluded: only the mixer window should claim to be one.
    pub const ALL: [Topic; 7] = [
        Topic::State,
        Topic::Meters,
        Topic::Devices,
        Topic::Apps,
        Topic::Engine,
        Topic::Settings,
        Topic::Hotkeys,
    ];
}

/// Serde helper distinguishing "field absent" (`None`) from "field set to
/// null" (`Some(None)`), used to clear optional settings such as a device.
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Read a present field, `null` included, as `Some`.
    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }

    /// Write `Some(None)` as `null`. `None` is left out by
    /// `skip_serializing_if`.
    pub fn serialize<T, S>(v: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error>
    where
        T: Serialize,
        S: Serializer,
    {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }
}

/// Put `v` in `slot`, when there is one: how a patch changes only what it
/// names.
fn set<T>(slot: &mut T, v: Option<T>) {
    if let Some(v) = v {
        *slot = v;
    }
}

/// Switch `slot` as `flag` says, when there is one.
fn switch(slot: &mut bool, flag: Option<Flag>) {
    if let Some(f) = flag {
        *slot = f.apply(*slot);
    }
}

/// An on/off setting in an update: `true`, `false`, or `"toggle"` to flip
/// whatever it is now. Toggling lets a button switch something without
/// first asking what it is, and without racing another client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    /// Switch it on (`true`) or off (`false`).
    Set(bool),
    /// Switch it to the opposite of what it is now.
    Toggle,
}

impl Flag {
    /// The value after applying this to `current`.
    pub fn apply(self, current: bool) -> bool {
        match self {
            Flag::Set(v) => v,
            Flag::Toggle => !current,
        }
    }
}

impl schemars::JsonSchema for Flag {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Flag".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "true, false, or \"toggle\" to flip whatever it is now",
            "oneOf": [{ "type": "boolean" }, { "const": "toggle" }]
        })
    }
}

impl From<bool> for Flag {
    fn from(v: bool) -> Self {
        Flag::Set(v)
    }
}

impl Serialize for Flag {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Flag::Set(v) => s.serialize_bool(*v),
            Flag::Toggle => s.serialize_str("toggle"),
        }
    }
}

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, schemars::JsonSchema)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Word(String),
        }
        match Raw::deserialize(d)? {
            Raw::Bool(v) => Ok(Flag::Set(v)),
            Raw::Word(w) => match w.to_ascii_lowercase().as_str() {
                "toggle" => Ok(Flag::Toggle),
                "on" | "true" => Ok(Flag::Set(true)),
                "off" | "false" => Ok(Flag::Set(false)),
                _ => Err(serde::de::Error::custom(format!(
                    "expected true, false or \"toggle\", got \"{w}\""
                ))),
            },
        }
    }
}

/// Partial update of an equalizer. Absent fields are left unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EqPatch {
    /// Switch the equalizer on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// Replaces every band. At most [`EQ_MAX_BANDS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<EqBand>>,
}

impl EqPatch {
    /// Change `eq` as this says, then bring it into range.
    pub fn apply(&self, eq: &mut Equalizer) {
        switch(&mut eq.enabled, self.enabled);
        set(&mut eq.bands, self.bands.clone());
        eq.normalize();
    }
}

/// Partial update of a noise gate. Absent fields are left unchanged; see
/// [`Gate`] for what each one does and its range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GatePatch {
    /// See [`Gate::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// See [`Gate::threshold_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_db: Option<f32>,
    /// See [`Gate::range_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_db: Option<f32>,
    /// See [`Gate::attack_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_ms: Option<f32>,
    /// See [`Gate::hold_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<f32>,
    /// See [`Gate::release_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_ms: Option<f32>,
}

impl GatePatch {
    /// Change `gate` as this says, then bring it into range.
    pub fn apply(&self, gate: &mut Gate) {
        switch(&mut gate.enabled, self.enabled);
        set(&mut gate.threshold_db, self.threshold_db);
        set(&mut gate.range_db, self.range_db);
        set(&mut gate.attack_ms, self.attack_ms);
        set(&mut gate.hold_ms, self.hold_ms);
        set(&mut gate.release_ms, self.release_ms);
        gate.normalize();
    }
}

/// A patch that sets every field to what `gate` has.
impl From<Gate> for GatePatch {
    fn from(gate: Gate) -> Self {
        Self {
            enabled: Some(Flag::Set(gate.enabled)),
            threshold_db: Some(gate.threshold_db),
            range_db: Some(gate.range_db),
            attack_ms: Some(gate.attack_ms),
            hold_ms: Some(gate.hold_ms),
            release_ms: Some(gate.release_ms),
        }
    }
}

/// Partial update of noise suppression. Absent fields are left unchanged;
/// see [`Denoise`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DenoisePatch {
    /// See [`Denoise::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// See [`Denoise::amount`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f32>,
}

impl DenoisePatch {
    /// Change `denoise` as this says, then bring it into range.
    pub fn apply(&self, denoise: &mut Denoise) {
        switch(&mut denoise.enabled, self.enabled);
        set(&mut denoise.amount, self.amount);
        denoise.normalize();
    }
}

/// A patch that sets every field to what `denoise` has.
impl From<Denoise> for DenoisePatch {
    fn from(denoise: Denoise) -> Self {
        Self {
            enabled: Some(Flag::Set(denoise.enabled)),
            amount: Some(denoise.amount),
        }
    }
}

/// Partial update of a compressor. Absent fields are left unchanged; see
/// [`Compressor`] for what each one does and its range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompressorPatch {
    /// See [`Compressor::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// See [`Compressor::threshold_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_db: Option<f32>,
    /// See [`Compressor::ratio`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    /// See [`Compressor::attack_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_ms: Option<f32>,
    /// See [`Compressor::release_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_ms: Option<f32>,
    /// See [`Compressor::makeup_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub makeup_db: Option<f32>,
    /// See [`Compressor::auto_makeup`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_makeup: Option<bool>,
}

impl CompressorPatch {
    /// Change `comp` as this says, then bring it into range.
    pub fn apply(&self, comp: &mut Compressor) {
        switch(&mut comp.enabled, self.enabled);
        set(&mut comp.threshold_db, self.threshold_db);
        set(&mut comp.ratio, self.ratio);
        set(&mut comp.attack_ms, self.attack_ms);
        set(&mut comp.release_ms, self.release_ms);
        set(&mut comp.makeup_db, self.makeup_db);
        set(&mut comp.auto_makeup, self.auto_makeup);
        comp.normalize();
    }
}

/// A patch that sets every field to what `comp` has.
impl From<Compressor> for CompressorPatch {
    fn from(comp: Compressor) -> Self {
        Self {
            enabled: Some(Flag::Set(comp.enabled)),
            threshold_db: Some(comp.threshold_db),
            ratio: Some(comp.ratio),
            attack_ms: Some(comp.attack_ms),
            release_ms: Some(comp.release_ms),
            makeup_db: Some(comp.makeup_db),
            auto_makeup: Some(comp.auto_makeup),
        }
    }
}

/// Partial update of ducking. Absent fields are left unchanged; `triggers`
/// and `buses` replace the whole set when given. See [`Ducking`] for what
/// each one does and its range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DuckingPatch {
    /// See [`Ducking::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// See [`Ducking::triggers`]. Strips may be given by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggers: Option<BTreeSet<StripId>>,
    /// See [`Ducking::amount_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_db: Option<f32>,
    /// See [`Ducking::buses`]. Buses may be given by name or label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buses: Option<BTreeSet<BusId>>,
    /// See [`Ducking::threshold_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_db: Option<f32>,
    /// See [`Ducking::attack_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_ms: Option<f32>,
    /// See [`Ducking::hold_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<f32>,
    /// See [`Ducking::release_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_ms: Option<f32>,
}

impl DuckingPatch {
    /// Change `duck` as this says, then bring it into range. Which strips
    /// and buses exist is checked when the mixer is normalized.
    pub fn apply(&self, duck: &mut Ducking) {
        switch(&mut duck.enabled, self.enabled);
        set(&mut duck.triggers, self.triggers.clone());
        set(&mut duck.buses, self.buses.clone());
        set(&mut duck.amount_db, self.amount_db);
        set(&mut duck.threshold_db, self.threshold_db);
        set(&mut duck.attack_ms, self.attack_ms);
        set(&mut duck.hold_ms, self.hold_ms);
        set(&mut duck.release_ms, self.release_ms);
        duck.normalize();
    }
}

/// A patch that sets every field to what `duck` has.
impl From<Ducking> for DuckingPatch {
    fn from(duck: Ducking) -> Self {
        Self {
            enabled: Some(Flag::Set(duck.enabled)),
            triggers: Some(duck.triggers),
            amount_db: Some(duck.amount_db),
            buses: Some(duck.buses),
            threshold_db: Some(duck.threshold_db),
            attack_ms: Some(duck.attack_ms),
            hold_ms: Some(duck.hold_ms),
            release_ms: Some(duck.release_ms),
        }
    }
}

/// Partial update of a limiter. Absent fields are left unchanged; see
/// [`Limiter`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LimiterPatch {
    /// See [`Limiter::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// See [`Limiter::ceiling_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_db: Option<f32>,
    /// See [`Limiter::release_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_ms: Option<f32>,
}

impl LimiterPatch {
    /// Change `limiter` as this says, then bring it into range.
    pub fn apply(&self, limiter: &mut Limiter) {
        switch(&mut limiter.enabled, self.enabled);
        set(&mut limiter.ceiling_db, self.ceiling_db);
        set(&mut limiter.release_ms, self.release_ms);
        limiter.normalize();
    }
}

/// A patch that sets every field to what `limiter` has.
impl From<Limiter> for LimiterPatch {
    fn from(limiter: Limiter) -> Self {
        Self {
            enabled: Some(Flag::Set(limiter.enabled)),
            ceiling_db: Some(limiter.ceiling_db),
            release_ms: Some(limiter.release_ms),
        }
    }
}

/// Partial update of a strip's or bus's external effects. Absent fields are
/// left unchanged; see [`Insert`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InsertPatch {
    /// See [`Insert::enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// See [`Insert::position`]. Strips and buses each have places of their
    /// own; asking a strip for a bus's place, or the other way round, is an
    /// error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<InsertPoint>,
    /// See [`Insert::fallback`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<InsertFallback>,
}

impl InsertPatch {
    /// Change `insert` as this says.
    pub fn apply(&self, insert: &mut Insert) {
        switch(&mut insert.enabled, self.enabled);
        set(&mut insert.position, self.position);
        set(&mut insert.fallback, self.fallback);
    }
}

/// A patch that sets every field to what `insert` has.
impl From<Insert> for InsertPatch {
    fn from(insert: Insert) -> Self {
        Self {
            enabled: Some(Flag::Set(insert.enabled)),
            position: Some(insert.position),
            fallback: Some(insert.fallback),
        }
    }
}

/// Partial update of a bus's downmix. Absent fields are left unchanged; see
/// [`Downmix`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DownmixPatch {
    /// See [`Downmix::method`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<DownmixMethod>,
    /// See [`Downmix::center_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center_db: Option<f32>,
    /// See [`Downmix::surround_db`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surround_db: Option<f32>,
    /// See [`Downmix::lfe`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lfe: Option<Flag>,
}

impl DownmixPatch {
    /// Change `downmix` as this says, then bring it into range.
    pub fn apply(&self, downmix: &mut Downmix) {
        set(&mut downmix.method, self.method);
        set(&mut downmix.center_db, self.center_db);
        set(&mut downmix.surround_db, self.surround_db);
        switch(&mut downmix.lfe, self.lfe);
        downmix.normalize();
    }
}

/// Partial update of a strip. Absent fields are left unchanged; see
/// [`Strip`] for what each one does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StripPatch {
    /// The strip to change, by id or by name.
    pub id: StripId,
    /// A new name: unique among strips, 40 characters at most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Set the fader, in dB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f32>,
    /// Move the fader by this many dB, after `gain_db` if both are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_delta_db: Option<f32>,
    /// Move the pan by this much, after `pan` if both are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan_delta: Option<f32>,
    /// See [`Strip::mute`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<Flag>,
    /// See [`Strip::solo`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solo: Option<Flag>,
    /// See [`Strip::pan`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<f32>,
    /// See [`Strip::layout`]; 1 to 16 channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<ChannelLayout>,
    /// Send levels to change, by bus, in dB. Buses not listed keep theirs.
    /// Buses may be given by name or label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends: Option<BTreeMap<BusId, f32>>,
    /// See [`Strip::upmix`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upmix: Option<Upmix>,
    /// See [`Strip::subwoofer`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subwoofer: Option<Flag>,
    /// The source a hardware strip captures from, by `node.name`; `null`
    /// for none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    #[schemars(with = "Option<Option<String>>")]
    pub device: Option<Option<String>>,
    /// An accent color, `#rrggbb`; `null` for none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    #[schemars(with = "Option<Option<String>>")]
    pub color: Option<Option<String>>,
    /// Change its equalizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<EqPatch>,
    /// Change its noise gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GatePatch>,
    /// Change its noise suppression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise: Option<DenoisePatch>,
    /// Change its compressor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compressor: Option<CompressorPatch>,
    /// Change its ducking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ducking: Option<DuckingPatch>,
    /// Change its external effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert: Option<InsertPatch>,
}

/// Partial update of a bus. Absent fields are left unchanged; see [`Bus`]
/// for what each one does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BusPatch {
    /// The bus to change, by id, label (`A1`) or name.
    pub id: BusId,
    /// A new name: unique among buses, 40 characters at most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Set the fader, in dB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f32>,
    /// Move the fader by this many dB, after `gain_db` if both are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_delta_db: Option<f32>,
    /// See [`Bus::mute`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<Flag>,
    /// See [`Bus::mono`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono: Option<Flag>,
    /// See [`Bus::delay_ms`]; 0 to [`BUS_DELAY_MAX_MS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<f32>,
    /// Move the delay by this many milliseconds, after `delay_ms` if both
    /// are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_delta_ms: Option<f32>,
    /// See [`Bus::layout`]; 1 to 16 channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<ChannelLayout>,
    /// The device a hardware bus plays to, by `node.name`; `null` for none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    #[schemars(with = "Option<Option<String>>")]
    pub device: Option<Option<String>>,
    /// An accent color, `#rrggbb`; `null` for none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    #[schemars(with = "Option<Option<String>>")]
    pub color: Option<Option<String>>,
    /// Change its equalizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<EqPatch>,
    /// Change its limiter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limiter: Option<LimiterPatch>,
    /// Change its downmix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downmix: Option<DownmixPatch>,
    /// Change its external effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert: Option<InsertPatch>,
}

/// Parameters of `set_route`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RouteParams {
    /// The strip, by id or name.
    pub strip: StripId,
    /// The bus, by id, label or name.
    pub bus: BusId,
    /// `true` / `false` to set. Absent toggles, unless `level_db` is given,
    /// in which case the route stays as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Flag>,
    /// The strip's level in this bus's mix, in dB, on top of its fader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_db: Option<f32>,
    /// Change that level by this many dB, after `level_db` if both are
    /// given. Like `level_db`, it leaves the route on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_delta_db: Option<f32>,
}

/// Parameters of `add_strip`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddStripParams {
    /// Unique among strips, 40 characters at most.
    pub name: String,
    /// A virtual playback device, or a capture from a hardware source.
    pub kind: StripKind,
    /// Its channels; stereo when not given.
    #[serde(default)]
    pub layout: ChannelLayout,
    /// For a hardware strip: the source to capture from, by `node.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Buses to route to initially. Absent: no routes.
    #[serde(default)]
    pub routes: Vec<BusId>,
}

/// Parameters of `add_bus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddBusParams {
    /// Unique among buses, 40 characters at most.
    pub name: String,
    /// Playing to a device, or a virtual microphone.
    pub kind: BusKind,
    /// Its channels; stereo when not given.
    #[serde(default)]
    pub layout: ChannelLayout,
    /// For a hardware bus: the device to play to, by `node.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// Parameters naming one strip or bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IdParams {
    /// Its id, or its name.
    pub id: u32,
}

/// The whole list of application rules, first match wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppRulesParams {
    /// Every rule. A second rule for the same application is dropped.
    pub rules: Vec<AppRule>,
}

/// Move a strip or bus to another place in the list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveParams {
    /// The strip or bus, by id or name.
    pub id: u32,
    /// Where it ends up, counting from 0. Past the end means last.
    pub index: usize,
}

/// Parameters of `move_app`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveAppParams {
    /// PipeWire node id of the application stream.
    pub app: u32,
    /// The virtual strip to move it to, by id or name.
    pub strip: StripId,
}

/// Change an application stream's own volume. Absent fields are unchanged,
/// and at least one must be given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppVolumeParams {
    /// PipeWire node id of the application stream.
    pub app: u32,
    /// Set the volume, in dB: 0 is full volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f32>,
    /// Change the volume by this many dB, after `volume_db` if both are
    /// given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_delta_db: Option<f32>,
    /// Mute or unmute the stream itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<Flag>,
}

/// Change the daemon's own settings. Absent fields are left unchanged; see
/// [`Settings`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SettingsPatch {
    /// See [`Settings::meter_rate_hz`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter_rate_hz: Option<u32>,
    /// See [`Settings::startup`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<Startup>,
    /// Start Weir's service when you log in, or stop doing so. Takes
    /// effect from the next login; the service running now carries on
    /// either way. An error where [`Settings::start_at_login`] is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at_login: Option<Flag>,
    /// See [`Settings::tray`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tray: Option<bool>,
    /// See [`Settings::tray_icon`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tray_icon: Option<TrayIcon>,
    /// See [`Settings::solo`]. A cue bus may be given by name or label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solo: Option<SoloMode>,
    /// From 8000 to 384000 Hz; `0` lets PipeWire decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// From 16 to 8192 frames; `0` lets PipeWire decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantum: Option<u32>,
}

/// Parameters naming a scene, a setup or an equalizer preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NameParams {
    /// Its name. Scene and setup names keep to letters, digits, spaces and
    /// `_ - .`, 64 characters at most.
    pub name: String,
}

/// Parameters of `save_eq_preset`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SaveEqPresetParams {
    /// The preset's name, 1 to 40 characters. Saving under the name of a
    /// preset of your own replaces it; built-in names are taken.
    pub name: String,
    /// Its bands, at most [`EQ_MAX_BANDS`].
    pub bands: Vec<EqBand>,
}

/// Apply an equalizer preset to exactly one of a strip or a bus. This
/// replaces its bands and switches its equalizer on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApplyEqPresetParams {
    /// The preset, by name, ignoring case.
    pub name: String,
    /// The strip, by id or name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<StripId>,
    /// The bus, by id, label or name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<BusId>,
}

/// The strips and buses whose equalizer spectrum this connection wants.
/// Replaces the previous list; an empty list stops them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WatchSpectrumParams {
    /// Up to 32 strips and buses, as `{"strip": 2}` or `{"bus": "B1"}`.
    #[serde(default)]
    pub targets: Vec<StripOrBus>,
}

/// Parameters of `subscribe` and `unsubscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubscribeParams {
    /// The topics to start or stop. Empty means [`Topic::ALL`] for
    /// `subscribe`, and every topic for `unsubscribe`.
    #[serde(default)]
    pub topics: Vec<Topic>,
    /// Send this connection meters at most this many times a second, fewer
    /// than the daemon makes. Nothing is lost in between: each message
    /// carries the peaks since the previous one sent here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter_rate_hz: Option<u32>,
}

/// Every method a client can call. Serialized as `{"method": ..., "params": ...}`.
///
/// A request lives for one call, so the size of its largest variant (a strip
/// update) costs nothing worth boxing it for.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Handshake. Returns [`HelloResult`].
    Hello,
    /// Returns [`FullState`].
    GetState,
    /// Returns `Vec<DeviceInfo>`.
    ListDevices,
    /// Returns `Vec<AppStream>`.
    ListApps,
    /// Returns the updated [`Strip`].
    SetStrip(StripPatch),
    /// Returns the updated [`Bus`].
    SetBus(BusPatch),
    /// Returns the updated [`Strip`].
    SetRoute(RouteParams),
    /// Returns the new [`Strip`].
    AddStrip(AddStripParams),
    /// Returns `null`.
    RemoveStrip(IdParams),
    /// Returns the new [`Bus`].
    AddBus(AddBusParams),
    /// Returns `null`.
    RemoveBus(IdParams),
    /// Replace every application rule. Returns `Vec<AppRule>`.
    SetAppRules(AppRulesParams),
    /// Returns the ids of every strip, in their new order.
    MoveStrip(MoveParams),
    /// Returns the ids of every bus, in their new order.
    MoveBus(MoveParams),
    /// Returns `null`.
    MoveApp(MoveAppParams),
    /// Returns `null`.
    SetAppVolume(AppVolumeParams),
    /// Returns the updated [`Settings`].
    SetSettings(SettingsPatch),
    /// Bring the mixer window to the front, starting it if it is not
    /// running. Returns `null`.
    ShowWindow,
    /// Returns `Vec<String>`, the saved setups.
    ListSetups,
    /// Save the whole mixer as a setup. Returns [`Library`].
    SaveSetup(NameParams),
    /// Switch to a setup, keeping the mix of every strip and bus that is in
    /// both. Returns the new [`MixerState`].
    LoadSetup(NameParams),
    /// Returns [`Library`].
    DeleteSetup(NameParams),
    /// Returns `Vec<String>`, the saved scenes.
    ListScenes,
    /// Save the current mix as a scene. Returns [`Library`].
    SaveScene(NameParams),
    /// Bring back a scene's mix. Returns the new [`MixerState`].
    LoadScene(NameParams),
    /// Returns [`Library`].
    DeleteScene(NameParams),
    /// Returns `Vec<EqPreset>`, built-in presets first.
    ListEqPresets,
    /// Saves, or replaces, a preset of your own. Returns `Vec<EqPreset>`.
    SaveEqPreset(SaveEqPresetParams),
    /// Deletes a preset of your own. Returns `Vec<EqPreset>`.
    DeleteEqPreset(NameParams),
    /// Returns the updated [`Strip`] or [`Bus`].
    ApplyEqPreset(ApplyEqPresetParams),
    /// Returns [`HotkeysInfo`]: every hotkey, and how keys reach Weir.
    ListHotkeys,
    /// Adds a hotkey, or replaces the one with the same `id`. Returns the
    /// saved [`Hotkey`], with its id.
    SetHotkey(Hotkey),
    /// Removes a hotkey. Returns [`HotkeysInfo`].
    RemoveHotkey(HotkeyRef),
    /// Switches a hotkey's keys on or off, leaving the rest of it as it
    /// is. Returns the [`Hotkey`].
    SwitchHotkey(SwitchHotkeyParams),
    /// Moves a hotkey to another place in the list, or into another group.
    /// Returns [`HotkeysInfo`].
    MoveHotkey(MoveHotkeyParams),
    /// Adds a group of hotkeys, last in the list. Returns the
    /// [`HotkeyGroup`], with its id.
    AddHotkeyGroup(AddHotkeyGroupParams),
    /// Renames a group of hotkeys, or switches it on or off. Returns the
    /// [`HotkeyGroup`].
    SetHotkeyGroup(SetHotkeyGroupParams),
    /// Removes a group of hotkeys. Its hotkeys stay, in no group. Returns
    /// [`HotkeysInfo`].
    RemoveHotkeyGroup(HotkeyGroupRef),
    /// Moves a group of hotkeys to another place in the list. Returns
    /// [`HotkeysInfo`].
    MoveHotkeyGroup(MoveHotkeyGroupParams),
    /// Does what pressing the hotkey's keys does, until `release_hotkey`.
    /// Returns the [`Hotkey`] once its steps are done.
    PressHotkey(HotkeyRef),
    /// Does what letting go of the hotkey's keys does. Returns the
    /// [`Hotkey`] once its steps are done.
    ReleaseHotkey(HotkeyRef),
    /// Presses the hotkey and lets go at once, like a tap on its keys.
    /// Returns the [`Hotkey`] once its steps are done.
    RunHotkey(HotkeyRef),
    /// Opens the desktop's shortcut settings at Weir's hotkeys, where their
    /// keys can be changed and more added. Only when [`KeysStatus`] says
    /// `configurable`. Returns `null`.
    OpenShortcutSettings,
    /// Start or stop `spectrum` notifications. Returns the targets now
    /// watched.
    WatchSpectrum(WatchSpectrumParams),
    /// Returns `Vec<Topic>` now active for this connection.
    Subscribe(SubscribeParams),
    /// Returns `Vec<Topic>` still active for this connection.
    Unsubscribe(SubscribeParams),
    /// Step back through recent changes to the mixer. Returns
    /// [`HistoryInfo`].
    Undo(HistoryStepParams),
    /// Step forward again after `undo`. Returns [`HistoryInfo`].
    Redo(HistoryStepParams),
    /// Returns [`HistoryInfo`].
    History,
    /// Returns JSON Schemas of every request, every notification and the
    /// full state, generated from the very types the daemon uses. See
    /// [`describe`].
    Describe,
    /// Returns `"pong"`.
    Ping,
}

/// `value` as JSON, for a response. Unlike `serde_json::to_value`, this
/// keeps each 32-bit float as short as it is: `6.7` rather than the
/// `6.699999809265137` a 64-bit float would make of it.
pub fn to_json<T: Serialize + ?Sized>(value: &T) -> Value {
    let text = serde_json::to_string(value).expect("value serializes");
    serde_json::from_str(&text).expect("serde_json reads what it writes")
}

/// What `describe` returns: `{ "protocol_version", "requests",
/// "notifications", "state" }`, the last three JSON Schemas (draft 2020-12).
/// `requests` is one schema per method, `{ "method": ..., "params": ... }`,
/// each with the method's documentation, which says what it returns.
pub fn describe() -> Value {
    serde_json::json!({
        "protocol_version": crate::PROTOCOL_VERSION,
        "requests": schemars::schema_for!(Request),
        "notifications": schemars::schema_for!(Notification),
        "state": schemars::schema_for!(FullState),
    })
}

/// Parameters of `undo` and `redo`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryStepParams {
    /// How many steps to take, from 1 to 100; 1 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
}

/// One change to the mixer that can be undone or redone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryEntry {
    /// What the change was, for people: "Music fader", "Remove strip Game".
    pub label: String,
    /// When it was made, in milliseconds since the Unix epoch.
    pub at_ms: u64,
}

/// What can be undone and redone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryInfo {
    /// Most recent first: `undo` takes back the first one.
    pub undo: Vec<HistoryEntry>,
    /// Next first: `redo` brings back the first one.
    pub redo: Vec<HistoryEntry>,
}

/// What `hello` returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HelloResult {
    /// [`crate::PROTOCOL_VERSION`] of the daemon. It changes only when the
    /// protocol changes in a way old clients would trip over.
    pub protocol_version: u32,
    /// The daemon's own version, such as `1.0.0`.
    pub daemon_version: String,
    /// Features this daemon has, such as `ducking` or `spectrum`, for
    /// clients that want to work with older daemons too.
    pub capabilities: Vec<String>,
}

/// Server to client notifications. Serialized as `{"method": ..., "params": ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Notification {
    /// The mixer changed: the whole new [`MixerState`].
    StateChanged(MixerState),
    /// Peak levels since the previous `meters`.
    Meters(Meters),
    /// The devices hardware strips and buses can use changed.
    DevicesChanged(Vec<DeviceInfo>),
    /// An application started or stopped playing, or moved.
    AppsChanged(Vec<AppStream>),
    /// The engine's status changed.
    EngineChanged(EngineStatus),
    /// The daemon's settings changed.
    SettingsChanged(Settings),
    /// The list of equalizer presets changed.
    EqPresetsChanged(Vec<EqPreset>),
    /// A strip's or bus's equalizer input and output, for an analyzer.
    Spectrum(Spectrum),
    /// Come to the front. Sent when the tray's "Show the mixer" is used and a
    /// window is already open.
    ShowWindow,
    /// Close now. Sent before the daemon restarts or exits, so the window
    /// does not outlive the mixer it is showing.
    Quit,
    /// What can be undone and redone changed.
    HistoryChanged(HistoryInfo),
    /// The application rules changed.
    AppRulesChanged(Vec<AppRule>),
    /// A scene or setup was saved, deleted or loaded.
    LibraryChanged(Library),
    /// The system volume of one of Weir's virtual devices changed.
    SystemVolumesChanged(SystemVolumes),
    /// Whether the external effects of each strip and bus that has them on
    /// are connected changed.
    InsertsChanged(Vec<InsertStatus>),
    /// The hotkeys, or how keys reach Weir, changed.
    HotkeysChanged(HotkeysInfo),
}

impl Notification {
    /// The topic a connection subscribes to for this notification.
    pub fn topic(&self) -> Topic {
        match self {
            Self::StateChanged(_)
            | Self::EqPresetsChanged(_)
            | Self::HistoryChanged(_)
            | Self::AppRulesChanged(_)
            | Self::LibraryChanged(_) => Topic::State,
            Self::Meters(_) => Topic::Meters,
            Self::DevicesChanged(_) | Self::SystemVolumesChanged(_) | Self::InsertsChanged(_) => {
                Topic::Devices
            }
            Self::AppsChanged(_) => Topic::Apps,
            Self::EngineChanged(_) => Topic::Engine,
            Self::SettingsChanged(_) => Topic::Settings,
            Self::Spectrum(_) => Topic::Spectrum,
            Self::ShowWindow | Self::Quit => Topic::Window,
            Self::HotkeysChanged(_) => Topic::Hotkeys,
        }
    }

    /// The notification as one line of the protocol, `jsonrpc` included,
    /// without the newline.
    pub fn to_wire(&self) -> String {
        #[derive(Serialize)]
        struct Wire<'a> {
            jsonrpc: &'static str,
            #[serde(flatten)]
            notification: &'a Notification,
        }
        let wire = Wire {
            jsonrpc: JSONRPC_VERSION,
            notification: self,
        };
        serde_json::to_string(&wire).expect("notification serializes")
    }
}

/// Anything that can arrive from the server: a response or a notification.
#[derive(Debug, Clone)]
pub enum ServerMessage {
    /// The answer to a request.
    Response(RpcResponse),
    /// Something that happened.
    Notification(Notification),
}

impl ServerMessage {
    /// Read one line from the server.
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        let v: Value = serde_json::from_str(line)?;
        if v.get("id").is_some() && (v.get("result").is_some() || v.get("error").is_some()) {
            Ok(Self::Response(serde_json::from_value(v)?))
        } else {
            Ok(Self::Notification(serde_json::from_value(v)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request::SetStrip(StripPatch {
            id: 3,
            gain_db: Some(-6.0),
            device: Some(None),
            ..Default::default()
        });
        let env = RpcRequest::new(1, &req);
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"method\":\"set_strip\""));
        assert!(json.contains("\"device\":null"));
        let back: RpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.parse().unwrap(), req);
    }

    #[test]
    fn optional_params_may_be_left_out() {
        let env: RpcRequest = serde_json::from_str(r#"{"id":1,"method":"undo"}"#).unwrap();
        assert_eq!(
            env.parse().unwrap(),
            Request::Undo(HistoryStepParams::default())
        );
        // Required parameters are still required.
        let env: RpcRequest = serde_json::from_str(r#"{"id":1,"method":"set_strip"}"#).unwrap();
        assert_eq!(env.parse().unwrap_err().code, RpcError::INVALID_PARAMS);
    }

    #[test]
    fn methods_without_parameters_accept_empty_ones() {
        for params in ["{}", "[]", "null"] {
            let env: RpcRequest = serde_json::from_str(&format!(
                r#"{{"id":1,"method":"get_state","params":{params}}}"#
            ))
            .unwrap();
            assert_eq!(env.parse().unwrap(), Request::GetState, "{params}");
        }
    }

    #[test]
    fn unit_method_without_params() {
        let env: RpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"hello"}"#).unwrap();
        assert_eq!(env.parse().unwrap(), Request::Hello);
        let env: RpcRequest = serde_json::from_str(r#"{"id":"x","method":"nope"}"#).unwrap();
        assert_eq!(env.parse().unwrap_err().code, RpcError::METHOD_NOT_FOUND);
    }

    #[test]
    fn a_value_nobody_knows_is_a_parameter_problem_not_an_unknown_method() {
        let env: RpcRequest = serde_json::from_str(
            r#"{"id":1,"method":"add_bus","params":{"name":"X","kind":"hardwear"}}"#,
        )
        .unwrap();
        let e = env.parse().unwrap_err();
        assert_eq!(e.code, RpcError::INVALID_PARAMS, "{e}");
        assert!(e.message.contains("hardwear"), "{e}");
    }

    #[test]
    fn patch_absent_vs_null() {
        let p: StripPatch = serde_json::from_str(r#"{"id":1}"#).unwrap();
        assert_eq!(p.device, None);
        let p: StripPatch = serde_json::from_str(r#"{"id":1,"device":null}"#).unwrap();
        assert_eq!(p.device, Some(None));
        let p: StripPatch = serde_json::from_str(r#"{"id":1,"device":"mic"}"#).unwrap();
        assert_eq!(p.device, Some(Some("mic".into())));
    }

    #[test]
    fn effect_patches_touch_only_what_they_name() {
        let p: StripPatch =
            serde_json::from_str(r#"{"id":1,"gate":{"enabled":true,"threshold_db":-30}}"#).unwrap();
        let mut gate = Gate {
            hold_ms: 500.0,
            ..Gate::default()
        };
        p.gate.as_ref().unwrap().apply(&mut gate);
        assert!(gate.enabled);
        assert_eq!(gate.threshold_db, -30.0);
        assert_eq!(gate.hold_ms, 500.0);
        assert!(p.eq.is_none() && p.denoise.is_none());

        let mut eq = Equalizer {
            enabled: false,
            bands: vec![EqBand::new(EqBandKind::Peak, 1000.0, 3.0, 1.0)],
        };
        EqPatch {
            enabled: Some(true.into()),
            bands: None,
        }
        .apply(&mut eq);
        assert!(eq.enabled);
        assert_eq!(eq.bands.len(), 1);
    }

    #[test]
    fn a_whole_effect_makes_a_patch_that_restores_it() {
        let comp = Compressor {
            enabled: true,
            threshold_db: -30.0,
            ratio: 6.0,
            auto_makeup: false,
            makeup_db: 4.0,
            ..Compressor::default()
        };
        let mut other = Compressor::default();
        CompressorPatch::from(comp).apply(&mut other);
        assert_eq!(other, comp);

        let duck = Ducking {
            enabled: true,
            triggers: [2].into(),
            buses: [1].into(),
            ..Ducking::default()
        };
        let mut other = Ducking::default();
        DuckingPatch::from(duck.clone()).apply(&mut other);
        assert_eq!(other, duck);
    }

    #[test]
    fn switches_take_true_false_or_toggle() {
        let p: StripPatch =
            serde_json::from_str(r#"{"id": 1, "mute": "toggle", "solo": true}"#).unwrap();
        assert_eq!(p.mute, Some(Flag::Toggle));
        assert_eq!(p.solo, Some(Flag::Set(true)));
        assert!(Flag::Toggle.apply(false));
        assert!(!Flag::Toggle.apply(true));
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["mute"], "toggle");
        assert_eq!(back["solo"], true);
        assert!(serde_json::from_str::<StripPatch>(r#"{"id": 1, "mute": "maybe"}"#).is_err());
    }

    #[test]
    fn floats_are_written_as_short_as_they_are() {
        let band = EqBand::new(EqBandKind::LowShelf, 100.0, 6.7, 0.707);
        let v = to_json(&band);
        assert_eq!(
            v.to_string(),
            r#"{"enabled":true,"freq_hz":100.0,"gain_db":6.7,"kind":"low_shelf","q":0.707}"#
        );
        let line = Notification::Meters(Meters {
            strips: [(1, vec![-12.3])].into(),
            ..Default::default()
        })
        .to_wire();
        assert!(line.contains("[-12.3]"), "{line}");
    }

    #[test]
    fn notification_wire() {
        let n = Notification::EngineChanged(EngineStatus::default());
        let line = n.to_wire();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "engine_changed");
        assert!(!line.contains('\n'));
        let quit: Value = serde_json::from_str(&Notification::Quit.to_wire()).unwrap();
        assert_eq!(
            quit,
            serde_json::json!({"jsonrpc": "2.0", "method": "quit"})
        );
        let msg = ServerMessage::parse(&line).unwrap();
        assert!(matches!(
            msg,
            ServerMessage::Notification(Notification::EngineChanged(_))
        ));
        let resp = RpcResponse::ok(Value::from(1), "pong");
        let msg = ServerMessage::parse(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert!(matches!(msg, ServerMessage::Response(_)));
    }
}
