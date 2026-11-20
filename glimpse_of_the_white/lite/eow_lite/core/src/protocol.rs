use std::io;

use eow_lite_helper::Payload;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::config::invalid;

pub const CORE_SCHEMA: &str = "core@1.1";
pub const SILENT_TAG: &str = "<k.action:silent/>";

#[derive(Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Request {
    #[serde(rename = "text.inbound")]
    Inbound { permanent: bool, inbound: Inbound },
    #[serde(rename = "text.push_prompt")]
    PushPrompt { module: String, prompt: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inbound {
    pub context_id: String,
    pub peer_type: String,
    pub peer_kind: String,
    pub peer_name: String,
    pub time: String,
    pub silent: bool,
    pub content: String,
}

impl Inbound {

    pub fn compact(&self, request_id: &str) -> io::Result<String> {
        if self.context_id != request_id {
            return Err(invalid("context_id must equal the request event_id"));
        }
        let time: OffsetDateTime = OffsetDateTime::parse(&self.time, &Rfc3339)
            .map_err(|_| invalid("inbound time must be an ISO 8601 UTC timestamp"))?;
        if time.offset() != UtcOffset::UTC {
            return Err(invalid("inbound time must be UTC"));
        }
        #[derive(Serialize)]
        struct Content<'a> {
            peer_type: &'a str,
            peer_kind: &'a str,
            peer_name: &'a str,
            time: &'a str,
            silent: bool,
            content: &'a str,
        }
        serde_json::to_string(&Content {
            peer_type: &self.peer_type,
            peer_kind: &self.peer_kind,
            peer_name: &self.peer_name,
            time: &self.time,
            silent: self.silent,
            content: &self.content,
        })
        .map_err(io::Error::other)
    }

}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Text,
    Tag,
}

#[derive(Serialize)]
#[serde(tag = "context_type", rename_all = "snake_case")]
pub enum ContextEvent<'a> {
    Inbound {
        context_id: &'a str,
        content: &'a str,
    },
    Outbound {
        context_id: &'a str,
        chunk_id: usize,
        content_type: ContentType,
        content: &'a str,
    },
    OutboundEnd {
        context_id: &'a str,
    },
}

pub fn context_payload(event: ContextEvent<'_>) -> io::Result<Payload> {
    match serde_json::to_value(event).map_err(io::Error::other)? {
        Value::Object(payload) => Ok(payload),
        _ => Err(invalid("context event payload must be an object")),
    }
}

pub fn response_payload(error: Option<&str>) -> Payload {
    [
        ("ok".into(), json!(error.is_none())),
        (
            "result".into(),
            error.map_or_else(|| json!({}), |reason: &str| json!({ "reason": reason })),
        ),
    ]
    .into_iter()
    .collect()
}
