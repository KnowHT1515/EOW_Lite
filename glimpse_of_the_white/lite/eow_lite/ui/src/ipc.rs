use std::io;

use eow_lite_helper::tcp::{read_frame, write_frame};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncWrite};
use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED};

pub async fn send<T: Serialize>(writer: &mut (impl AsyncWrite + Unpin), message: &T) -> io::Result<()> {
    let body: Vec<u8> = serde_json::to_vec(message).map_err(io::Error::other)?;
    write_frame(writer, &body).await
}

pub async fn receive<T: DeserializeOwned>(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<T>> {
    let body: Option<Vec<u8>> = match read_frame(reader).await {
        Ok(body) => body,
        Err(error) if disconnected(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    match body {
        Some(body) => serde_json::from_slice(&body).map(Some).map_err(io::Error::other),
        None => Ok(None),
    }
}

pub fn disconnected(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected | io::ErrorKind::UnexpectedEof)
        || error.raw_os_error().is_some_and(|code: i32| matches!(code as u32, ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED))
}
