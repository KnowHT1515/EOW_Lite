use std::{io, path::PathBuf, sync::Arc, time::Duration};

use eow_lite_helper::{Binding, EnvelopeKind, EnvelopeLite, EnvelopeLiteKind, ModuleRegistry, Payload, SendResult};
use eow_lite_helper::lifecycle::{ModuleLifecycle, announce};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::launch::Session;
use crate::protocol::{ClientMessage, ContextEvent, ServerMessage};

pub async fn run(session: &mut Session) -> io::Result<()> {
    let lifecycle: ModuleLifecycle = ModuleLifecycle::load(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."))?;
    let name: String = lifecycle.module_name("ui")?.to_owned();
    let (registry, shutdown): (ModuleRegistry, _) = lifecycle.into_parts();
    let registry: Arc<ModuleRegistry> = Arc::new(registry);
    let (signal, mut signals): (mpsc::UnboundedSender<()>, mpsc::UnboundedReceiver<()>) = mpsc::unbounded_channel();
    ctrlc::set_handler(move || {
        let _ = signal.send(());
    }).map_err(io::Error::other)?;
    let work = async {
        let binding: Binding = Binding::connect(Arc::clone(&registry), "ui", Duration::from_secs(10)).await.map_err(io::Error::other)?;
        binding.subscribe("core.llm_context").await.map_err(io::Error::other)?;
        session.send(&ServerMessage::Ready).await?;
        announce(&name, "Online")?;
        receive(session, &binding, &registry).await
    };
    let result: io::Result<()> = tokio::select! {
        result = shutdown => result,
        _ = signals.recv() => Ok(()),
        result = work => result,
    };
    result?;
    announce(&name, "Offline")
}

async fn receive(session: &mut Session, binding: &Binding, registry: &ModuleRegistry) -> io::Result<()> {
    let mut pending: Option<String> = None;
    loop {
        tokio::select! {
            message = session.incoming.recv() => {
                match message.transpose()? {
                    Some(ClientMessage::Input { content }) => {
                        if pending.is_some() {
                            session.send(&ServerMessage::Notice { message: "Waiting for the current response".into() }).await?;
                            continue;
                        }
                        if content.trim().is_empty() {
                            continue;
                        }
                        let envelope: EnvelopeLite = inbound(registry, &content)?;
                        let context_id: String = envelope.event_id.clone();
                        session.send(&ServerMessage::Input { context_id: context_id.clone(), content }).await?;
                        pending = Some(context_id.clone());
                        let result: SendResult = binding.send(&envelope).await.map_err(io::Error::other)?;
                        if !result.ok {
                            pending = None;
                            session.send(&ServerMessage::Rejected { context_id, reason: result.violation.join(", ") }).await?;
                        }
                    },
                    Some(ClientMessage::Ready) => {},
                    Some(ClientMessage::Close) | None => return Ok(()),
                }
            },
            envelope = binding.recv() => {
                let envelope: EnvelopeLite = envelope.map_err(io::Error::other)?.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Transport disconnected"))?;
                match envelope.envelope_kind {
                    EnvelopeKind::Response if envelope.correlation_id.as_ref() == pending.as_ref() => {
                        let payload: Payload = envelope.decode_payload().map_err(io::Error::other)?;
                        if payload.get("ok") == Some(&Value::Bool(false)) {
                            let reason: String = payload.get("result").and_then(|value: &Value| value.get("reason")).and_then(Value::as_str).unwrap_or("Request rejected").to_owned();
                            if let Some(context_id) = pending.take() {
                                session.send(&ServerMessage::Rejected { context_id, reason }).await?;
                            }
                        }
                    },
                    EnvelopeKind::Event if envelope.source == "core" && envelope.target == "core.llm_context" && envelope.payload_schema == "core@1.1" => {
                        let event: ContextEvent = serde_json::from_str(envelope.payload.get()).map_err(io::Error::other)?;
                        if let ContextEvent::OutboundEnd { context_id } = &event
                            && pending.as_ref() == Some(context_id) {
                            pending = None;
                        }
                        session.send(&ServerMessage::Context { event }).await?;
                    },
                    _ => {},
                }
            },
        }
    }
}

fn inbound(registry: &ModuleRegistry, content: &str) -> io::Result<EnvelopeLite> {
    let source: &eow_lite_helper::EowLiteModule = registry.get("ui").ok_or_else(|| io::Error::other("missing UI module"))?;
    let mut envelope: EnvelopeLite = EnvelopeLite::build(EnvelopeLiteKind::Request, "core", source, "1.1", Payload::new()).map_err(io::Error::other)?;
    envelope.payload = serde_json::value::to_raw_value(&json!({
        "operation": "text.inbound",
        "permanent": true,
        "inbound": {
            "context_id": envelope.event_id,
            "peer_type": "human",
            "peer_kind": "external",
            "peer_name": "anonymous",
            "time": envelope.ts_utc,
            "silent": false,
            "content": content,
        },
    })).map_err(io::Error::other)?;
    Ok(envelope)
}
