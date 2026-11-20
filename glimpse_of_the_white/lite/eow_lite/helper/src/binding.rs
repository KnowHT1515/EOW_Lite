use std::{fmt, io, net::Ipv4Addr, sync::{Arc, Mutex as StateMutex}, time::Duration};

use tokio::{net::{TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}}, sync::{Mutex, mpsc, oneshot}, task::JoinHandle, time::timeout};

use crate::control::{HANDSHAKE_SCHEMA, Handshake, HandshakeDenyReason, HandshakeReply, HandshakeRequest, HandshakeResult, SendResult, SendResultFrame, SubscriptionReply, SubscriptionRequest};
use crate::envelope::{EnvelopeLite, valid_topic};
use crate::registry::ModuleRegistry;
use crate::tcp::{MAX_FRAME_BODY, read_frame, write_frame};

#[derive(Debug)]
pub enum BindingError {
    InvalidEnvelope(Vec<String>),
    Denied(HandshakeDenyReason),
    Closed,
    Timeout,
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for BindingError {

    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEnvelope(fields) => write!(f, "invalid envelope: {}", fields.join(", ")),
            Self::Denied(reason) => write!(f, "handshake denied: {reason:?}"),
            Self::Closed => f.write_str("transport connection closed"),
            Self::Timeout => f.write_str("transport operation timed out"),
            Self::Io(err) => err.fmt(f),
            Self::Json(err) => err.fmt(f),
        }
    }

}

impl std::error::Error for BindingError {}

impl From<io::Error> for BindingError {

    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }

}

impl From<serde_json::Error> for BindingError {

    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }

}

#[derive(Default)]
struct ReadState {
    closed: bool,
    pending: Option<oneshot::Sender<OperationReply>>,
}

enum OperationReply {
    Send(SendResult),
    Subscription(SubscriptionReply),
}

impl ReadState {

    fn close(&mut self) {
        self.closed = true;
        self.pending = None;
    }

}

pub struct Binding {
    registry: Arc<ModuleRegistry>,
    module: String,
    deadline: Duration,
    writer: Arc<Mutex<Option<OwnedWriteHalf>>>,
    state: Arc<StateMutex<ReadState>>,
    incoming: Mutex<mpsc::UnboundedReceiver<Result<EnvelopeLite, BindingError>>>,
    reader: JoinHandle<()>,
}

impl Binding {

    pub async fn connect(registry: Arc<ModuleRegistry>, module: &str, deadline: Duration) -> Result<Self, BindingError> {
        let port: u16 = registry
            .get("transport")
            .and_then(|entry: &crate::registry::EowLiteModule| entry.port().parse::<u16>().ok())
            .filter(|port: &u16| *port != 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid transport port"))?;
        let stream: TcpStream = timeout(deadline, async {
            let mut stream: TcpStream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
            let request: Handshake<HandshakeRequest> = Handshake {
                handshake: HandshakeRequest {
                    schema: HANDSHAKE_SCHEMA.into(),
                    module: module.into(),
                },
            };
            write_frame(&mut stream, &serde_json::to_vec(&request)?).await?;
            let body: Vec<u8> = read_frame(&mut stream).await?.ok_or(BindingError::Closed)?;
            let reply: Handshake<HandshakeReply> = serde_json::from_slice(&body)?;
            match (reply.handshake.result, reply.handshake.reason) {
                (HandshakeResult::Accept, None) => Ok(stream),
                (HandshakeResult::Deny, Some(reason)) => Err(BindingError::Denied(reason)),
                _ => Err(
                    io::Error::new(io::ErrorKind::InvalidData, "invalid handshake reply").into(),
                ),
            }
        })
        .await
        .map_err(|_| BindingError::Timeout)??;

        let (read_half, write_half): (OwnedReadHalf, OwnedWriteHalf) = stream.into_split();
        let writer: Arc<Mutex<Option<OwnedWriteHalf>>> = Arc::new(Mutex::new(Some(write_half)));
        let state: Arc<StateMutex<ReadState>> = Arc::new(StateMutex::new(ReadState::default()));
        let (sender, incoming) = mpsc::unbounded_channel();
        let reader: JoinHandle<()> = {
            let writer: Arc<Mutex<Option<OwnedWriteHalf>>> = Arc::clone(&writer);
            let state: Arc<StateMutex<ReadState>> = Arc::clone(&state);
            tokio::spawn(async move {
                let result: Result<(), BindingError> = receive_frames(read_half, &state, &sender).await;
                state.lock().unwrap().close();
                writer.lock().await.take();
                if let Err(err) = result {
                    let _ = sender.send(Err(err));
                }
            })
        };
        Ok(Self {
            registry,
            module: module.into(),
            deadline,
            writer,
            state,
            incoming: Mutex::new(incoming),
            reader,
        })
    }

    pub async fn send(&self, envelope: &EnvelopeLite) -> Result<SendResult, BindingError> {
        let violations: Vec<String> = envelope.validate_header(&self.registry, &self.module);
        if !violations.is_empty() {
            return Err(BindingError::InvalidEnvelope(violations));
        }
        if let Err(err) = envelope.validate_payload() {
            return Err(BindingError::InvalidEnvelope(vec![err.to_string()]));
        }
        let body: Vec<u8> = serde_json::to_vec(envelope)?;
        match self.exchange(&body).await? {
            OperationReply::Send(result) => Ok(result),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected subscription reply").into()),
        }
    }

    pub async fn subscribe(&self, topic: &str) -> Result<(), BindingError> {
        self.subscription(SubscriptionRequest::Subscribe { topic: topic.into() }).await
    }

    pub async fn unsubscribe(&self, topic: &str) -> Result<(), BindingError> {
        self.subscription(SubscriptionRequest::Unsubscribe { topic: topic.into() }).await
    }

    async fn subscription(&self, request: SubscriptionRequest) -> Result<(), BindingError> {
        if !valid_topic(request.topic(), &self.registry) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid subscription topic").into());
        }
        let expected: SubscriptionReply = request.reply();
        let body: Vec<u8> = serde_json::to_vec(&request)?;
        match self.exchange(&body).await? {
            OperationReply::Subscription(reply) if reply == expected => Ok(()),
            OperationReply::Send(result) if !result.ok => Err(BindingError::InvalidEnvelope(result.violation)),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected subscription reply").into()),
        }
    }

    async fn exchange(&self, body: &[u8]) -> Result<OperationReply, BindingError> {
        if body.len() > MAX_FRAME_BODY {
            return Err(BindingError::InvalidEnvelope(vec![
                "frame body length".into(),
            ]));
        }

        let mut slot: tokio::sync::MutexGuard<'_, Option<OwnedWriteHalf>> = self.writer.lock().await;
        let (sender, ack): (oneshot::Sender<OperationReply>, oneshot::Receiver<OperationReply>) = oneshot::channel();
        let mut send: PendingWrite<'_> = PendingWrite {
            binding: self,
            writer: Some(slot.take().ok_or(BindingError::Closed)?),
        };
        {
            let mut state: std::sync::MutexGuard<'_, ReadState> = self.state.lock().unwrap();
            if state.closed {
                return Err(BindingError::Closed);
            }
            state.pending = Some(sender);
        }

        let result: OperationReply = timeout(self.deadline, async {
            write_frame(send.writer.as_mut().unwrap(), body).await?;
            ack.await.map_err(|_| BindingError::Closed)
        })
        .await
        .map_err(|_| BindingError::Timeout)??;
        *slot = send.writer.take();
        Ok(result)
    }

    pub async fn recv(&self) -> Result<Option<EnvelopeLite>, BindingError> {
        self.incoming.lock().await.recv().await.transpose()
    }

}

impl Drop for Binding {

    fn drop(&mut self) {
        self.reader.abort();
    }

}

struct PendingWrite<'a> {
    binding: &'a Binding,
    writer: Option<OwnedWriteHalf>,
}

impl Drop for PendingWrite<'_> {

    fn drop(&mut self) {
        if self.writer.is_some() {
            self.binding.state.lock().unwrap().close();
            self.binding.reader.abort();
        }
    }

}

async fn receive_frames(mut reader: OwnedReadHalf, state: &StateMutex<ReadState>, incoming: &mpsc::UnboundedSender<Result<EnvelopeLite, BindingError>>) -> Result<(), BindingError> {
    while let Some(body) = read_frame(&mut reader).await? {
        let reply: Option<OperationReply> = if let Ok(frame) = serde_json::from_slice::<SendResultFrame>(&body) {
            Some(OperationReply::Send(frame.send_result))
        }
        else if let Ok(reply) = serde_json::from_slice::<SubscriptionReply>(&body) {
            Some(OperationReply::Subscription(reply))
        }
        else {
            None
        };
        if let Some(reply) = reply {
            let pending: oneshot::Sender<OperationReply> = state.lock().unwrap().pending.take().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "unexpected operation reply")
            })?;
            let _ = pending.send(reply);
        }
        else {
            let envelope: EnvelopeLite = serde_json::from_slice::<EnvelopeLite>(&body)?;
            if incoming.send(Ok(envelope)).is_err() {
                break;
            }
        }
    }
    Ok(())
}
