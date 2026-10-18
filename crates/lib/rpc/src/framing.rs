//! Newline-delimited JSON framing, the same framing MCP uses over stdio.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::Message;

/// Largest accepted message. Bounds memory per connection; file contents
/// returned by actions are capped well below this.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("message exceeds {MAX_MESSAGE_BYTES} bytes")]
    TooLarge,
    #[error("malformed message: {0}")]
    Malformed(String),
}

/// Reads one line (without its newline). `Ok(None)` at a clean end of stream.
///
/// Oversized lines are an error: the stream can't be resynchronized safely, so
/// the connection should be dropped.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(FrameError::Malformed("stream ended mid-message".into()))
            };
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], Some(i + 1)),
            None => (available, None),
        };
        if line.len() + chunk.len() > MAX_MESSAGE_BYTES {
            return Err(FrameError::TooLarge);
        }
        line.extend_from_slice(chunk);
        let consumed = done.unwrap_or(chunk.len());
        reader.consume(consumed);
        if done.is_some() {
            if line.ends_with(b"\r") {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

/// Reads the next message, skipping blank lines.
pub async fn read_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Result<Message, String>>, FrameError> {
    loop {
        let Some(line) = read_line(reader).await? else {
            return Ok(None);
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return Ok(Some(
            serde_json::from_slice(&line).map_err(|e| e.to_string()),
        ));
    }
}

pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), FrameError> {
    let mut bytes =
        serde_json::to_vec(message).map_err(|e| FrameError::Malformed(e.to_string()))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(FrameError::TooLarge);
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn reads_lines_and_skips_blanks() {
        let input: &[u8] =
            b"{\"jsonrpc\":\"2.0\",\"method\":\"a\"}\r\n\n{\"jsonrpc\":\"2.0\",\"method\":\"b\"}\n";
        let mut reader = BufReader::new(input);
        let a = read_message(&mut reader).await.unwrap().unwrap().unwrap();
        let b = read_message(&mut reader).await.unwrap().unwrap().unwrap();
        assert!(matches!(a, Message::Request(r) if r.method == "a"));
        assert!(matches!(b, Message::Request(r) if r.method == "b"));
        assert!(read_message(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn malformed_json_is_reported_not_fatal() {
        let input: &[u8] = b"not json\n";
        let mut reader = BufReader::new(input);
        assert!(read_message(&mut reader).await.unwrap().unwrap().is_err());
    }

    #[tokio::test]
    async fn oversized_lines_are_rejected() {
        let input = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut reader = BufReader::new(input.as_slice());
        assert!(matches!(
            read_line(&mut reader).await,
            Err(FrameError::TooLarge)
        ));
    }
}
