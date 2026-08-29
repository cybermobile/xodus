use crate::simple_context::SimpleContext;

pub async fn handle(
    _socket: &mut tokio::net::UnixStream,
    _context: &mut SimpleContext,
) -> tokio::io::Result<()> {
    // No framing is defined for this transport yet, so the stream cannot be
    // resynchronized; the router closes the connection on error.
    Err(tokio::io::Error::other(
        "the protobuf transport is not implemented yet; use the XML transport",
    ))
}
