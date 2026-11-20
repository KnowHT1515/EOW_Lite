use std::{future::pending, io, sync::Arc, time::Duration};

use eow_lite_helper::{Binding, EnvelopeKind, EnvelopeLite, EnvelopeLiteKind, ModuleRegistry, Payload};
use futures_util::StreamExt;
use tokio::{task::JoinHandle, time::timeout};

use crate::config::{LoadedConfig, invalid};
use crate::context::{History, Message};
use crate::http;
use crate::http::{ModelClient, SseDecoder};
use crate::prompt::Prompt;
use crate::protocol::{CORE_SCHEMA, ContextEvent, Inbound, Request, SILENT_TAG, context_payload, response_payload};
use crate::splitter::{SentenceSplitter, SplitCandidate, SplitterConfig};

pub struct Runtime {
    channel: Channel,
    model: ModelClient,
    prompt: Prompt,
    splitter: Arc<SplitterConfig>,
    history: History,
    active: Option<JoinHandle<io::Result<CompletedTurn>>>,
}

impl Runtime {

    pub fn new(binding: Binding, registry: Arc<ModuleRegistry>, config: LoadedConfig) -> io::Result<Self> {
        Ok(Self {
            channel: Channel {
                binding: Arc::new(binding),
                registry,
            },
            model: ModelClient::new(config.model)?,
            prompt: Prompt::new(config.prompt, config.core_prompt),
            splitter: config.splitter,
            history: History::default(),
            active: None,
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        loop {
            tokio::select! {
                biased;
                completed = async {
                    match self.active.as_mut() {
                        Some(task) => task.await,
                        None => pending().await,
                    }
                } => {
                    self.active.take();
                    match completed {
                        Ok(Ok(turn)) if turn.permanent => self.history.commit(turn.inbound, turn.outbound),
                        Ok(Ok(_)) => {},
                        Ok(Err(error)) => eprintln!("core: {error}"),
                        Err(_) => return Err(invalid("Core turn task failed")),
                    }
                },
                envelope = self.channel.binding.recv() => {
                    let envelope: EnvelopeLite = envelope.map_err(io::Error::other)?
                        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Transport disconnected"))?;
                    self.receive(envelope).await?;
                },
            }
        }
    }

    async fn receive(&mut self, envelope: EnvelopeLite) -> io::Result<()> {
        if envelope.envelope_kind != EnvelopeKind::Request {
            return Ok(());
        }
        if envelope.payload_schema != CORE_SCHEMA {
            return self.channel
                .reply(&envelope, Some("unsupported Core payload schema"))
                .await;
        }
        let request: Request = match serde_json::from_str(envelope.payload.get()) {
            Ok(request) => request,
            Err(_) => {
                return self.channel
                    .reply(&envelope, Some("invalid Core request fields or operation"))
                    .await;
            },
        };
        match request {
            Request::PushPrompt { module, prompt } => {
                let error: Option<String> = self.prompt
                    .push(&envelope.source, module, prompt)
                    .err()
                    .map(|error: io::Error| error.to_string());
                self.channel.reply(&envelope, error.as_deref()).await
            },
            Request::Inbound { permanent, inbound } => {
                if self.active.is_some() {
                    return self.channel.reply(&envelope, Some("busy")).await;
                }
                let compact: String = match inbound.compact(&envelope.event_id) {
                    Ok(compact) => compact,
                    Err(error) => {
                        return self.channel
                            .reply(&envelope, Some(&error.to_string()))
                            .await;
                    },
                };
                let messages: Vec<Message> = self.history.messages(&self.prompt.snapshot(), &compact);
                self.channel.reply(&envelope, None).await?;
                let turn: Turn = Turn {
                    channel: self.channel.clone(),
                    model: self.model.clone(),
                    splitter: Arc::clone(&self.splitter),
                    inbound,
                    compact,
                    messages,
                    permanent,
                };
                self.active = Some(tokio::spawn(turn.run()));
                Ok(())
            },
        }
    }

    pub async fn stop(&mut self) {
        if let Some(task) = self.active.take() {
            task.abort();
            let _ = task.await;
        }
    }

}

impl Drop for Runtime {

    fn drop(&mut self) {
        if let Some(task) = self.active.take() {
            task.abort();
        }
    }

}

#[derive(Clone)]
struct Channel {
    binding: Arc<Binding>,
    registry: Arc<ModuleRegistry>,
}

impl Channel {

    async fn send(&self, kind: EnvelopeLiteKind, target: &str, schema: &str, payload: Payload) -> io::Result<()> {
        let source: &eow_lite_helper::EowLiteModule = self.registry
            .get("core")
            .ok_or_else(|| invalid("missing core module"))?;
        let envelope: EnvelopeLite =
            EnvelopeLite::build(kind, target, source, schema, payload).map_err(io::Error::other)?;
        let receipt: eow_lite_helper::SendResult = self.binding
            .send(&envelope)
            .await
            .map_err(io::Error::other)?;
        if !receipt.ok {
            return Err(invalid(format!(
                "Transport rejected Core envelope: {}",
                receipt.violation.join(", ")
            )));
        }
        Ok(())
    }

    async fn reply(&self, request: &EnvelopeLite, error: Option<&str>) -> io::Result<()> {
        self.send(
            EnvelopeLiteKind::Response {
                correlation_id: request.event_id.clone(),
            },
            &request.source,
            "1.0",
            response_payload(error),
        )
        .await
    }

    async fn event(&self, event: ContextEvent<'_>) -> io::Result<()> {
        self.send(
            EnvelopeLiteKind::Event,
            "core.llm_context",
            "1.1",
            context_payload(event)?,
        )
        .await
    }

    async fn chunks(&self, context_id: &str, next_id: &mut usize, chunks: Vec<SplitCandidate>) -> io::Result<()> {
        for chunk in chunks {
            let (content_type, content): (crate::protocol::ContentType, String) = chunk.into_parts();
            self.event(ContextEvent::Outbound {
                context_id,
                chunk_id: *next_id,
                content_type,
                content: &content,
            })
            .await?;
            *next_id += 1;
        }
        Ok(())
    }

}

struct CompletedTurn {
    permanent: bool,
    inbound: String,
    outbound: String,
}

struct Turn {
    channel: Channel,
    model: ModelClient,
    splitter: Arc<SplitterConfig>,
    inbound: Inbound,
    compact: String,
    messages: Vec<Message>,
    permanent: bool,
}

impl Turn {

    async fn run(self) -> io::Result<CompletedTurn> {
        let result: io::Result<String> = self.generate().await;
        let end: io::Result<()> = self.channel
            .event(ContextEvent::OutboundEnd {
                context_id: &self.inbound.context_id,
            })
            .await;
        let outbound: String = result?;
        end?;
        Ok(CompletedTurn {
            permanent: self.permanent,
            inbound: self.compact,
            outbound,
        })
    }

    async fn generate(&self) -> io::Result<String> {
        self.channel
            .event(ContextEvent::Inbound {
                context_id: &self.inbound.context_id,
                content: &self.compact,
            })
            .await?;
        let mut next_id: usize = 0;
        if self.inbound.silent {
            self.publish_complete(SILENT_TAG, &mut next_id).await?;
            return Ok(SILENT_TAG.into());
        }
        let response: reqwest::Response = self.model.start(&self.messages).await?;
        if !self.model.streaming() {
            let outbound: String = http::complete(response).await?;
            self.publish_complete(&outbound, &mut next_id).await?;
            return Ok(outbound);
        }

        let mut stream = response.bytes_stream();
        let mut decoder: SseDecoder = SseDecoder::default();
        let mut splitter: SentenceSplitter = SentenceSplitter::new(Arc::clone(&self.splitter));
        let mut outbound: String = String::new();
        loop {
            let bytes = timeout(Duration::from_secs(30), stream.next())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "model stream idle timeout"))?
                .ok_or_else(|| invalid("model stream ended without a finish marker"))?
                .map_err(|_| invalid("model stream interrupted"))?;
            for data in decoder.feed(&bytes) {
                let delta: crate::http::Delta = http::decode_delta(&data)?;
                if let Some(content) = delta.content {
                    outbound.push_str(&content);
                    self.channel
                        .chunks(
                            &self.inbound.context_id,
                            &mut next_id,
                            splitter.push_str(&content),
                        )
                        .await?;
                }
                if delta.finished {
                    self.channel
                        .chunks(&self.inbound.context_id, &mut next_id, splitter.finish())
                        .await?;
                    return Ok(outbound);
                }
            }
        }
    }

    async fn publish_complete(&self, outbound: &str, next_id: &mut usize) -> io::Result<()> {
        self.channel
            .chunks(
                &self.inbound.context_id,
                next_id,
                SentenceSplitter::split(Arc::clone(&self.splitter), outbound),
            )
            .await
    }

}
