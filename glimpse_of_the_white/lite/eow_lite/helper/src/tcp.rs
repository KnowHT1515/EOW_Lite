use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BODY: usize = 20 * 1024 * 1024;
pub const SENTINEL: [u8; 5] = *b"\\eow/";

pub async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<Vec<u8>>> {
    let mut prefix = [0; 4];
    if reader.read(&mut prefix[..1]).await? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut prefix[1..]).await?;
    let length = u32::from_be_bytes(prefix) as usize;
    validate_length(length)?;

    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    let mut sentinel = [0; SENTINEL.len()];
    reader.read_exact(&mut sentinel).await?;
    if sentinel != SENTINEL {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame sentinel",
        ));
    }
    Ok(Some(body))
}

pub async fn write_frame(writer: &mut (impl AsyncWrite + Unpin), body: &[u8]) -> io::Result<()> {
    validate_length(body.len())?;
    writer.write_all(&(body.len() as u32).to_be_bytes()).await?;
    writer.write_all(body).await?;
    writer.write_all(&SENTINEL).await
}

fn validate_length(length: usize) -> io::Result<()> {
    if length == 0 || length > MAX_FRAME_BODY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame body length",
        ));
    }
    Ok(())
}
