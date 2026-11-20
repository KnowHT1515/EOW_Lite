use std::{io, time::Duration};

use reqwest::{Client, Response, Url, redirect};
use serde::Serialize;
use serde_json::Value;
use tokio::time::timeout;

use crate::config::{ModelConfig, invalid};
use crate::context::Message;

#[derive(Clone)]
pub struct ModelClient {
    client: Client,
    config: ModelConfig,
    temperature: Option<f64>,
}

impl ModelClient {

    pub fn new(config: ModelConfig) -> io::Result<Self> {
        let url: Url = Url::parse(&config.url).map_err(|_| invalid("invalid LLM_Model.url"))?;
        if !matches!(url.scheme(), "http" | "https") || !url.has_host() || config.model.is_empty() {
            return Err(invalid("LLM_Model requires an HTTP URL and model ID"));
        }
        let temperature: Option<f64> = config.temperature
            .as_ref()
            .map(|value: &String| {
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|value: &f64| value.is_finite() && (0.0..=2.0).contains(value))
                    .ok_or_else(|| {
                        invalid("LLM_Model.temperature must be a number between 0 and 2")
                    })
            })
            .transpose()?;
        let client: Client = Client::builder()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(if config.stream { 180 } else { 120 }))
            .build()
            .map_err(|_| invalid("could not create model HTTP client"))?;
        Ok(Self {
            client,
            config,
            temperature,
        })
    }

    pub fn streaming(&self) -> bool {
        self.config.stream
    }

    pub async fn start(&self, messages: &[Message]) -> io::Result<Response> {
        let body: ChatBody<'_> = ChatBody {
            model: &self.config.model,
            messages,
            stream: self.config.stream,
            temperature: self.temperature,
            thinking: self.config.thinking.map(|enabled: bool| Thinking {
                mode: if enabled { "enabled" } else { "disabled" },
            }),
        };
        let response: Response = timeout(
            Duration::from_secs(30),
            self.client
                .post(&self.config.url)
                .bearer_auth(&self.config.api_key)
                .header(
                    reqwest::header::ACCEPT,
                    if self.config.stream {
                        "text/event-stream"
                    }
                    else {
                        "application/json"
                    },
                )
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "model response headers timed out"))?
        .map_err(|_| invalid("model HTTP connection failed"))?;
        if !response.status().is_success() {
            return Err(invalid(format!(
                "model HTTP status {}",
                response.status().as_u16()
            )));
        }
        Ok(response)
    }

}

#[derive(Serialize)]
struct ChatBody<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
}

#[derive(Serialize)]
struct Thinking {
    #[serde(rename = "type")]
    mode: &'static str,
}

fn choice(document: &Value) -> io::Result<Option<&Value>> {
    let choices: &Vec<Value> = document
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("model response has no choices array"))?;
    let selected: Option<&Value> = choices
        .iter()
        .find(|value: &&Value| value.get("index").and_then(Value::as_u64) == Some(0))
        .or_else(|| {
            choices
                .iter()
                .all(|value: &Value| value.get("index").is_none())
                .then(|| choices.first())
                .flatten()
        });
    if !choices.is_empty() && selected.is_none() {
        return Err(invalid("model response has no choice with index zero"));
    }
    Ok(selected)
}

pub async fn complete(response: Response) -> io::Result<String> {
    let document: Value = response
        .json()
        .await
        .map_err(|_| invalid("invalid model JSON response"))?;
    choice(&document)?
        .and_then(|value: &Value| value.get("message"))
        .and_then(|value: &Value| value.get("content"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid("model response has no message content"))
}

pub struct Delta {
    pub content: Option<String>,
    pub finished: bool,
}

pub fn decode_delta(data: &[u8]) -> io::Result<Delta> {
    if data == b"[DONE]" {
        return Ok(Delta {
            content: None,
            finished: true,
        });
    }
    let document: Value =
        serde_json::from_slice(data).map_err(|_| invalid("invalid model SSE JSON"))?;
    let Some(choice): Option<&Value> = choice(&document)? else {
        return Ok(Delta {
            content: None,
            finished: false,
        });
    };
    let content: Option<String> = match choice.get("delta").and_then(|value: &Value| value.get("content")) {
        None | Some(Value::Null) => None,
        Some(Value::String(content)) => Some(content.clone()),
        Some(_) => return Err(invalid("invalid model delta content")),
    };
    let finished: bool = match choice.get("finish_reason") {
        None | Some(Value::Null) => false,
        Some(Value::String(_)) => true,
        Some(_) => return Err(invalid("invalid model finish_reason")),
    };
    Ok(Delta { content, finished })
}

#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    after_cr: bool,
}

impl SseDecoder {

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut events: Vec<Vec<u8>> = Vec::new();
        for &byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.finish_line(&mut events);
                    self.after_cr = true;
                },
                b'\n' => self.finish_line(&mut events),
                _ => self.line.push(byte),
            }
        }
        events
    }

    fn finish_line(&mut self, events: &mut Vec<Vec<u8>>) {
        let line: Vec<u8> = std::mem::take(&mut self.line);
        if line.is_empty() {
            if self.has_data {
                events.push(std::mem::take(&mut self.data));
                self.has_data = false;
            }
        }
        else if let Some(data) = line
            .strip_prefix(b"data:")
            .or_else(|| (line == b"data").then_some(&[][..]))
        {
            if self.has_data {
                self.data.push(b'\n');
            }
            self.data
                .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
            self.has_data = true;
        }
    }

}
