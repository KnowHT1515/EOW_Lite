use serde::{Deserialize, Serialize};

pub const HANDSHAKE_SCHEMA: &str = "1.0";

#[derive(Debug, Deserialize, Serialize)]
pub struct Handshake<T> {
    pub handshake: T,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HandshakeRequest {
    pub schema: String,
    pub module: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeResult {
    Accept,
    Deny,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeDenyReason {
    InvalidModule,
    ModuleInUse,
    SchemaIncompatible,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HandshakeReply {
    pub result: HandshakeResult,
    pub reason: Option<HandshakeDenyReason>,
}

impl HandshakeReply {

    pub fn accept() -> Self {
        Self {
            result: HandshakeResult::Accept,
            reason: None,
        }
    }

    pub fn deny(reason: HandshakeDenyReason) -> Self {
        Self {
            result: HandshakeResult::Deny,
            reason: Some(reason),
        }
    }

}

#[derive(Debug, Deserialize, Serialize)]
pub struct SendResultFrame {
    pub send_result: SendResult,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SendResult {
    pub ok: bool,
    pub violation: Vec<String>,
}
