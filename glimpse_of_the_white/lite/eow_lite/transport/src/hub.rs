use std::{collections::{HashMap, HashSet, hash_map::Entry}, io, sync::{Arc, Mutex}, time::Duration};

use eow_lite_helper::control::{HANDSHAKE_SCHEMA, Handshake, HandshakeDenyReason, HandshakeReply, SendResultFrame, SubscriptionReply, SubscriptionRequest};
use eow_lite_helper::envelope::valid_topic;
use eow_lite_helper::tcp::{read_frame, write_frame};
use eow_lite_helper::{EnvelopeKind, EnvelopeLite, EnvelopeLiteKind, ModuleRegistry, SendResult};
use serde_json::{Value, json};
use tokio::{net::{TcpListener, TcpStream}, sync::mpsc, time::timeout};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
type Outbound = mpsc::UnboundedSender<Vec<u8>>;

struct Hub {
    registry: ModuleRegistry,
    routing: Mutex<RoutingState>,
}

#[derive(Default)]
struct RoutingState {
    online: HashMap<String, Outbound>,
    subscriptions: HashMap<String, HashSet<String>>,
}

struct Registration<'a> {
    hub: &'a Hub,
    module: String,
}

impl Drop for Registration<'_> {

    fn drop(&mut self) {
        let mut routing: std::sync::MutexGuard<'_, RoutingState> = self.hub.routing.lock().unwrap();
        routing.online.remove(&self.module);
        routing.subscriptions.retain(|_, subscribers: &mut HashSet<String>| {
            subscribers.remove(&self.module);
            !subscribers.is_empty()
        });
    }

}

pub async fn serve(listener: TcpListener, registry: ModuleRegistry) -> io::Result<()> {
    let hub: Arc<Hub> = Arc::new(Hub {
        registry,
        routing: Mutex::new(RoutingState::default()),
    });
    loop {
        let (stream, peer): (TcpStream, std::net::SocketAddr) = listener.accept().await?;
        let hub: Arc<Hub> = Arc::clone(&hub);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, &hub).await {
                eprintln!("transport connection {peer}: {err}");
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, hub: &Hub) -> io::Result<()> {
    let body: Option<Vec<u8>> = timeout(HANDSHAKE_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))??;
    let Some(body): Option<Vec<u8>> = body else {
        return Ok(());
    };
    let request: Handshake<Value> = serde_json::from_slice(&body)?;
    let (out, mut queue): (Outbound, mpsc::UnboundedReceiver<Vec<u8>>) = mpsc::unbounded_channel();
    let registration: Registration<'_> = match hub.register(&request.handshake, &out) {
        Ok(registration) => registration,
        Err(reason) => {
            let reply: Vec<u8> = handshake_bytes(HandshakeReply::deny(reason))?;
            write_frame(&mut stream, &reply).await?;
            return Ok(());
        },
    };

    let (mut reader, mut writer): (tokio::net::tcp::ReadHalf<'_>, tokio::net::tcp::WriteHalf<'_>) = stream.split();
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

    fn register(&self, request: &Value, out: &Outbound) -> Result<Registration<'_>, HandshakeDenyReason> {
        if request.get("schema").and_then(Value::as_str) != Some(HANDSHAKE_SCHEMA) {
            return Err(HandshakeDenyReason::SchemaIncompatible);
        }
        let module: &str = request
            .get("module")
            .and_then(Value::as_str)
            .filter(|module: &&str| *module != "transport" && self.registry.get(module).is_some())
            .ok_or(HandshakeDenyReason::InvalidModule)?;
        let mut routing: std::sync::MutexGuard<'_, RoutingState> = self.routing.lock().unwrap();
        match routing.online.entry(module.into()) {
            Entry::Occupied(_) => Err(HandshakeDenyReason::ModuleInUse),
            Entry::Vacant(entry) => {
                let reply: Vec<u8> =
                    handshake_bytes(HandshakeReply::accept()).expect("handshake serialization");
                out.send(reply).expect("handshake queue receiver is live");
                entry.insert(out.clone());
                Ok(Registration {
                    hub: self,
                    module: module.into(),
                })
            },
        }
    }

    fn submit(&self, body: Vec<u8>, module: &str, out: &Outbound) -> io::Result<()> {
        if let Ok(request) = serde_json::from_slice::<SubscriptionRequest>(&body) {
            return self.subscription(request, module, out);
        }
        let envelope: EnvelopeLite = match serde_json::from_slice(&body) {
            Ok(envelope) => envelope,
            Err(_) => return send_result(out, vec!["invalid_envelope".into()]),
        };
        let violation: Vec<String> = envelope.validate_header(&self.registry, module);
        let accepted: bool = violation.is_empty();
        send_result(out, violation)?;
        if !accepted {
            return Ok(());
        }

        if envelope.envelope_kind == EnvelopeKind::Event {
            let routing: std::sync::MutexGuard<'_, RoutingState> = self.routing.lock().unwrap();
            if let Some(subscribers) = routing.subscriptions.get(&envelope.target) {
                for module in subscribers {
                    if let Some(subscriber) = routing.online.get(module) {
                        let _ = subscriber.send(body.clone());
                    }
                }
            }
            return Ok(());
        }

        let target: Option<Outbound> = self.routing.lock().unwrap().online.get(&envelope.target).cloned();
        if target.is_some_and(|target: Outbound| target.send(body).is_ok()) {
            return Ok(());
        }

        eprintln!(
            "transport: {} -> {} could not be delivered",
            module, envelope.target
        );
        if envelope.envelope_kind == EnvelopeKind::Request {
            let source: &eow_lite_helper::EowLiteModule = self.registry
                .get("transport")
                .expect("transport is registered");
            let target: &eow_lite_helper::EowLiteModule = self.registry
                .get(module)
                .expect("connection module is registered");
            let failure: EnvelopeLite = EnvelopeLite::build(
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

    fn subscription(&self, request: SubscriptionRequest, module: &str, out: &Outbound) -> io::Result<()> {
        if !valid_topic(request.topic(), &self.registry) {
            return send_result(out, vec!["topic".into()]);
        }
        let reply: SubscriptionReply = request.reply();
        let mut routing: std::sync::MutexGuard<'_, RoutingState> = self.routing.lock().unwrap();
        match request {
            SubscriptionRequest::Subscribe { topic } => {
                routing.subscriptions.entry(topic).or_default().insert(module.into());
            },
            SubscriptionRequest::Unsubscribe { topic } => {
                if let Some(subscribers) = routing.subscriptions.get_mut(&topic) {
                    subscribers.remove(module);
                    if subscribers.is_empty() {
                        routing.subscriptions.remove(&topic);
                    }
                }
            },
        }
        enqueue(out, serde_json::to_vec(&reply)?)
    }

}

fn handshake_bytes(reply: HandshakeReply) -> io::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Handshake { handshake: reply })?)
}

fn send_result(out: &Outbound, violation: Vec<String>) -> io::Result<()> {
    let send_result: SendResult = SendResult {
        ok: violation.is_empty(),
        violation,
    };
    enqueue(out, serde_json::to_vec(&SendResultFrame { send_result })?)
}

fn enqueue(out: &Outbound, body: Vec<u8>) -> io::Result<()> {
    out.send(body)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "connection writer closed"))
}
