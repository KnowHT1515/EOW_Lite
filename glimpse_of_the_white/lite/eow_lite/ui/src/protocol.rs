use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Ready,
    Input { content: String },
    Close,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready,
    Input { context_id: String, content: String },
    Context { event: ContextEvent },
    Rejected { context_id: String, reason: String },
    Notice { message: String },
    Close,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "context_type", rename_all = "snake_case")]
pub enum ContextEvent {
    Inbound {
        #[serde(default)]
        private: bool,
        context_id: String,
        content: String,
    },
    Outbound {
        context_id: String,
        #[serde(default)]
        private: bool,
        chunk_id: usize,
        content_type: ContentType,
        content: String,
    },
    OutboundEnd { context_id: String },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Text,
    Tag,
}
