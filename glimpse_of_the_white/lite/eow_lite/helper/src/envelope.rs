use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, value::RawValue};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::registry::{EowLiteModule, ModuleRegistry};

const ENVELOPE_SCHEMA: &str = "1.0";
const RESPONSE_SCHEMA: &str = "transport@1.0";
pub type Payload = Map<String, Value>;

#[derive(Debug)]
pub enum EnvelopeLiteKind {
    Request,
    Response { correlation_id: String },
    Event,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeKind {
    Request,
    Response,
    Event,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EnvelopeLite {
    pub envelope_schema: String,
    pub envelope_kind: EnvelopeKind,
    pub source: String,
    pub target: String,
    pub event_id: String,
    #[serde(deserialize_with = "deserialize_correlation_id")]
    pub correlation_id: Option<String>,
    pub ts_utc: String,
    pub payload_schema: String,
    pub payload: Box<RawValue>,
}

fn deserialize_correlation_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Debug)]
pub enum EnvelopeError {
    InvalidField(&'static str),
    Json(serde_json::Error),
}

impl fmt::Display for EnvelopeError {

    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(f, "invalid envelope field: {field}"),
            Self::Json(err) => err.fmt(f),
        }
    }

}

impl std::error::Error for EnvelopeError {}

impl EnvelopeLite {

    pub fn build(kind: EnvelopeLiteKind, target: &EowLiteModule, source: &EowLiteModule, schema: &str, payload: Payload) -> Result<Self, EnvelopeError> {
        let (envelope_kind, namespace, correlation_id): (EnvelopeKind, &str, Option<String>) = match kind {
            EnvelopeLiteKind::Request => (EnvelopeKind::Request, target.module(), None),
            EnvelopeLiteKind::Event => (EnvelopeKind::Event, source.module(), None),
            EnvelopeLiteKind::Response { correlation_id } => {
                (EnvelopeKind::Response, "transport", Some(correlation_id))
            },
        };
        let now: OffsetDateTime = OffsetDateTime::now_utc();
        let (hour, minute, second, millis): (u8, u8, u8, u16) = now.to_hms_milli();
        let suffix: i32 = rand::random_range(0..100);
        let envelope: Self = Self {
            envelope_schema: ENVELOPE_SCHEMA.into(),
            envelope_kind,
            source: source.module().into(),
            target: target.module().into(),
            event_id: format!(
                "7{}{hour:02}{minute:02}{second:02}{millis:03}{suffix:02}",
                source.number(),
            ),
            correlation_id,
            ts_utc: now
                .format(&Rfc3339)
                .map_err(|_| EnvelopeError::InvalidField("ts_utc"))?,
            payload_schema: format!("{namespace}@{schema}"),
            payload: serde_json::value::to_raw_value(&payload).map_err(EnvelopeError::Json)?,
        };
        if !envelope.valid_payload_schema() {
            return Err(EnvelopeError::InvalidField("payload_schema"));
        }
        envelope.validate_payload()?;
        Ok(envelope)
    }

    pub fn validate_header(&self, registry: &ModuleRegistry, source: &str) -> Vec<String> {
        let mut violation: Vec<String> = Vec::new();
        if self.envelope_schema != ENVELOPE_SCHEMA {
            violation.push("envelope_schema".into());
        }
        if self.source != source || registry.get(&self.source).is_none() {
            violation.push("source".into());
        }
        if registry.get(&self.target).is_none() {
            violation.push("target".into());
        }
        if !valid_event_id(&self.event_id)
            || !registry
                .modules()
                .iter()
                .any(|module: &EowLiteModule| module.number() == &self.event_id[1..3])
        {
            violation.push("event_id".into());
        }
        if !self.valid_correlation_id() {
            violation.push("correlation_id".into());
        }
        if !matches!(OffsetDateTime::parse(&self.ts_utc, &Rfc3339), Ok(ts) if ts.offset() == UtcOffset::UTC)
        {
            violation.push("ts_utc".into());
        }
        if !self.valid_payload_schema() {
            violation.push("payload_schema".into());
        }
        if !self.payload.get().trim_start().starts_with('{') {
            violation.push("payload".into());
        }
        violation
    }

    pub fn validate_payload(&self) -> Result<(), EnvelopeError> {
        if self.envelope_kind == EnvelopeKind::Response {
            if !self.correlation_id.as_deref().is_some_and(valid_event_id) {
                return Err(EnvelopeError::InvalidField("correlation_id"));
            }
            let payload: Payload = self.decode_payload()?;
            if !matches!(payload.get("ok"), Some(Value::Bool(_)))
                || !matches!(payload.get("result"), Some(Value::Object(_)))
            {
                return Err(EnvelopeError::InvalidField("payload"));
            }
        }
        Ok(())
    }

    pub fn decode_payload(&self) -> Result<Payload, EnvelopeError> {
        serde_json::from_str(self.payload.get()).map_err(EnvelopeError::Json)
    }

    fn valid_correlation_id(&self) -> bool {
        match self.envelope_kind {
            EnvelopeKind::Response => self.correlation_id
                .as_ref()
                .is_some_and(|id: &String| !id.is_empty()),
            _ => self.correlation_id.is_none(),
        }
    }

    fn valid_payload_schema(&self) -> bool {
        let Some((namespace, version)): Option<(&str, &str)> = self.payload_schema.split_once('@') else {
            return false;
        };
        match self.envelope_kind {
            EnvelopeKind::Request => namespace == self.target && valid_version(version),
            EnvelopeKind::Event => namespace == self.source && valid_version(version),
            EnvelopeKind::Response => self.payload_schema == RESPONSE_SCHEMA,
        }
    }

}

fn valid_version(version: &str) -> bool {
    version.split_once('.').is_some_and(|(major, minor): (&str, &str)| {
        [major, minor]
            .iter()
            .all(|part: &&str| !part.is_empty() && part.bytes().all(|b: u8| b.is_ascii_digit()))
    })
}

fn valid_event_id(id: &str) -> bool {
    let bytes: &[u8] = id.as_bytes();
    if bytes.len() != 14 || bytes[0] != b'7' || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let time_part = |i: usize| (bytes[i] - b'0') * 10 + (bytes[i + 1] - b'0');
    time_part(3) < 24 && time_part(5) < 60 && time_part(7) < 60
}
