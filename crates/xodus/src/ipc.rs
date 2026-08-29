//! Client-side helpers for the xodus-service Unix socket.
//!
//! Frame layout (all little-endian): `u32` magic, `u16` message type
//! ([`crate::proto::xodus::XodusMessageType`]), `u16` payload size, payload.
//! A successful response uses the request's message type + 1; failures use
//! `ERROR_RESPONSE` with an XML-serialized
//! [`crate::models::xgameruntime::xuser::ErrorResponse`] payload.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::proto::xodus::XodusMessageType;

pub const XML_MAGIC: u32 = 0x58445358;
pub const PROTO_MAGIC: u32 = 0x58445350;

/// Directory the service socket lives in. Must agree between the service
/// (which binds) and clients (which connect), so both use this.
pub fn runtime_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(PathBuf::from("/tmp"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
    }
}

pub fn socket_path() -> Option<PathBuf> {
    Some(runtime_dir()?.join("xodus.sock"))
}

pub fn encode_frame(magic: u32, msg_type: u16, payload: &[u8]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(8 + payload.len());
    buffer.extend(magic.to_le_bytes());
    buffer.extend(msg_type.to_le_bytes());
    buffer.extend((payload.len() as u16).to_le_bytes());
    buffer.extend(payload);
    buffer
}

/// Sends a `Ping` and waits for the echoed `Pong`, bounded by `timeout`.
/// `Ok(())` means a live xodus-service is answering on the socket.
pub async fn ping(path: &std::path::Path, timeout: Duration) -> std::io::Result<()> {
    tokio::time::timeout(timeout, async {
        let mut stream = UnixStream::connect(path).await?;
        let payload = b"xodus-ping";
        stream
            .write_all(&encode_frame(
                XML_MAGIC,
                XodusMessageType::Ping as u16,
                payload,
            ))
            .await?;

        let magic = stream.read_u32_le().await?;
        let msg_type = stream.read_u16_le().await?;
        let size = stream.read_u16_le().await?;
        let mut buffer = vec![0; size as usize];
        stream.read_exact(&mut buffer).await?;

        if magic != XML_MAGIC || msg_type != XodusMessageType::Pong as u16 || buffer != payload {
            return Err(std::io::Error::other("unexpected ping response"));
        }
        Ok(())
    })
    .await
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[test]
    fn frame_layout_is_magic_type_size_payload() {
        let frame = encode_frame(XML_MAGIC, 3, b"abc");
        assert_eq!(&frame[0..4], &XML_MAGIC.to_le_bytes());
        assert_eq!(&frame[4..6], &3u16.to_le_bytes());
        assert_eq!(&frame[6..8], &3u16.to_le_bytes());
        assert_eq!(&frame[8..], b"abc");
    }

    #[tokio::test]
    async fn ping_round_trips_against_a_service_shaped_server() {
        let dir = std::env::temp_dir().join(format!("xodus-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("xodus.sock");
        let _ = std::fs::remove_file(&path);

        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Mirror the service router: read magic/type/size/payload, reply
            // with <request type + 1> and the echoed payload.
            let magic = stream.read_u32_le().await.unwrap();
            let msg_type = stream.read_u16_le().await.unwrap();
            let size = stream.read_u16_le().await.unwrap();
            let mut payload = vec![0; size as usize];
            stream.read_exact(&mut payload).await.unwrap();
            assert_eq!(magic, XML_MAGIC);
            assert_eq!(msg_type, XodusMessageType::Ping as u16);
            stream
                .write_all(&encode_frame(magic, msg_type + 1, &payload))
                .await
                .unwrap();
        });

        ping(&path, Duration::from_secs(5)).await.unwrap();
        server.await.unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn ping_fails_fast_when_nothing_listens() {
        let path = std::env::temp_dir().join("xodus-ipc-test-nonexistent.sock");
        assert!(ping(&path, Duration::from_secs(1)).await.is_err());
    }
}
