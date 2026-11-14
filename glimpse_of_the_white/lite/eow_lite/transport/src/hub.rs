use std::collections::{HashMap, hash_map::Entry};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eow_lite_helper::control::{
    HANDSHAKE_SCHEMA, Handshake, HandshakeDenyReason, HandshakeReply, SendResultFrame,
};
use eow_lite_helper::tcp::{read_frame, write_frame};
use eow_lite_helper::{EnvelopeKind, EnvelopeLite, EnvelopeLiteKind, ModuleRegistry, SendResult};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
type Outbound = mpsc::UnboundedSender<Vec<u8>>;

struct Hub {
    registry: ModuleRegistry,
    online: Mutex<HashMap<String, Outbound>>,
}

struct Registration<'a> {
    hub: &'a Hub,
    module: String,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.hub.online.lock().unwrap().remove(&self.module);
    }
}

pub async fn serve(listener: TcpListener, registry: ModuleRegistry) -> io::Result<()> {
    let hub = Arc::new(Hub {
        registry,
        online: Mutex::new(HashMap::new()),
    });
    loop {
        let (stream, peer) = listener.accept().await?;
        let hub = Arc::clone(&hub);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, &hub).await {
                eprintln!("transport connection {peer}: {err}");
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, hub: &Hub) -> io::Result<()> {
    let body = timeout(HANDSHAKE_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))??;
    let Some(body) = body else {
        return Ok(());
    };
    let request: Handshake<Value> = serde_json::from_slice(&body)?;
    let (out, mut queue) = mpsc::unbounded_channel();
    let registration = match hub.register(&request.handshake, &out) {
        Ok(registration) => registration,
        Err(reason) => {
            let reply = handshake_bytes(HandshakeReply::deny(reason))?;
            write_frame(&mut stream, &reply).await?;
            return Ok(());
        }
    };

    let (mut reader, mut writer) = stream.split();
    let read = async {
        while let Some(body) = read_frame(&mut reader).await? {
            hub.submit(body, &registration.module, &out)?;
        }
        Ok(())
    };
    let write = async {
        while let Some(body) = queue.recv().await {
            write_frame(&mut writer, &body).await?;
        }
        Ok(())
    };
    tokio::select! {
        result = read => result,
        result = write => result,
    }
}

impl Hub {
    fn register(
        &self,
        request: &Value,
        out: &Outbound,
    ) -> Result<Registration<'_>, HandshakeDenyReason> {
        if request.get("schema").and_then(Value::as_str) != Some(HANDSHAKE_SCHEMA) {
            return Err(HandshakeDenyReason::SchemaIncompatible);
        }
        let module = request
            .get("module")
            .and_then(Value::as_str)
            .filter(|module| *module != "transport" && self.registry.get(module).is_some())
            .ok_or(HandshakeDenyReason::InvalidModule)?;
        let mut online = self.online.lock().unwrap();
        match online.entry(module.into()) {
            Entry::Occupied(_) => Err(HandshakeDenyReason::ModuleInUse),
            Entry::Vacant(entry) => {
                // Queue the accept before any other connection can route to this module.
                let reply =
                    handshake_bytes(HandshakeReply::accept()).expect("handshake serialization");
                out.send(reply).expect("handshake queue receiver is live");
                entry.insert(out.clone());
                Ok(Registration {
                    hub: self,
                    module: module.into(),
                })
            }
        }
    }

    fn submit(&self, body: Vec<u8>, module: &str, out: &Outbound) -> io::Result<()> {
        let envelope: EnvelopeLite = match serde_json::from_slice(&body) {
            Ok(envelope) => envelope,
            Err(_) => return send_result(out, vec!["invalid_envelope".into()]),
        };
        let violation = envelope.validate_header(&self.registry, module);
        let accepted = violation.is_empty();
        send_result(out, violation)?;
        if !accepted {
            return Ok(());
        }

        let target = self.online.lock().unwrap().get(&envelope.target).cloned();
        if target.is_some_and(|target| target.send(body).is_ok()) {
            return Ok(());
        }

        eprintln!(
            "transport: {} -> {} could not be delivered",
            module, envelope.target
        );
        if envelope.envelope_kind == EnvelopeKind::Request {
            let source = self
                .registry
                .get("transport")
                .expect("transport is registered");
            let target = self
                .registry
                .get(module)
                .expect("connection module is registered");
            let failure = EnvelopeLite::build(
                EnvelopeLiteKind::Response {
                    correlation_id: envelope.event_id,
                },
                target,
                source,
                "1.0",
                [
                    ("ok".into(), Value::Bool(false)),
                    ("result".into(), json!({ "reason": "target_offline" })),
                ]
                .into_iter()
                .collect(),
            )
            .map_err(io::Error::other)?;
            enqueue(out, serde_json::to_vec(&failure)?)?;
        }
        Ok(())
    }
}

fn handshake_bytes(reply: HandshakeReply) -> io::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Handshake { handshake: reply })?)
}

fn send_result(out: &Outbound, violation: Vec<String>) -> io::Result<()> {
    let send_result = SendResult {
        ok: violation.is_empty(),
        violation,
    };
    enqueue(out, serde_json::to_vec(&SendResultFrame { send_result })?)
}

fn enqueue(out: &Outbound, body: Vec<u8>) -> io::Result<()> {
    out.send(body)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "connection writer closed"))
}
