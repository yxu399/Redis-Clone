use crate::frame::Frame;
use anyhow::{anyhow, Result};
use bytes::{Buf, Bytes, BytesMut};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

pub struct RespCodec;

impl Decoder for RespCodec {
    type Item = Frame;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        if src.is_empty() {
            return Ok(None);
        }

        // 1. Peek at the first byte to determine type
        // We use a Cursor to track our position without consuming the buffer yet.
        // If we fail to parse a full frame, we discard the cursor and try again later.
        let mut cursor = std::io::Cursor::new(&src[..]);

        match check_frame(&mut cursor) {
            Ok(_) => {
                // Calculation: The check_frame advanced the cursor to the end of the frame.
                // We know exactly how long the frame is.
                let len = cursor.position() as usize;

                // RESET cursor to read the actual data
                cursor.set_position(0);

                // PARSE the data
                let frame = parse_frame(&mut cursor, src)?;

                // ADVANCE the buffer cursor, consuming the bytes from the stream
                src.advance(len);

                Ok(Some(frame))
            }
            Err(e) => {
                // Incomplete implies we need more bytes from the network
                if e.downcast_ref::<io::Error>()
                    .map_or(false, |e| e.kind() == io::ErrorKind::UnexpectedEof)
                {
                    Ok(None)
                } else {
                    Err(e) // Real data corruption error
                }
            }
        }
    }
}

impl Encoder<Frame> for RespCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> Result<()> {
        match frame {
            Frame::Simple(s) => {
                dst.extend_from_slice(b"+");
                dst.extend_from_slice(s.as_bytes());
                dst.extend_from_slice(b"\r\n");
            }
            Frame::Error(s) => {
                dst.extend_from_slice(b"-");
                dst.extend_from_slice(s.as_bytes());
                dst.extend_from_slice(b"\r\n");
            }
            Frame::Integer(n) => {
                dst.extend_from_slice(b":");
                // Convert integer to string bytes
                let val = n.to_string();
                dst.extend_from_slice(val.as_bytes());
                dst.extend_from_slice(b"\r\n");
            }
            Frame::Bulk(data) => {
                dst.extend_from_slice(b"$");
                let len = data.len().to_string();
                dst.extend_from_slice(len.as_bytes());
                dst.extend_from_slice(b"\r\n");
                dst.extend_from_slice(&data);
                dst.extend_from_slice(b"\r\n");
            }
            Frame::Null => {
                dst.extend_from_slice(b"$-1\r\n");
            }
            Frame::Array(items) => {
                dst.extend_from_slice(b"*");
                let len = items.len().to_string();
                dst.extend_from_slice(len.as_bytes());
                dst.extend_from_slice(b"\r\n");

                // Recursively encode each element
                for item in items {
                    self.encode(item, dst)?;
                }
            }
        }
        Ok(())
    }
}

// --- Parsing Logic Helpers ---

// Checks if a full frame is available in the buffer.
// Returns Ok(()) if yes, UnexpectedEof if not.
fn check_frame(cursor: &mut io::Cursor<&[u8]>) -> Result<()> {
    // Read type byte
    if !cursor.has_remaining() {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    let type_byte = cursor.get_u8();

    match type_byte {
        b'+' | b'-' | b':' => {
            get_line(cursor)?;
            Ok(())
        }
        b'$' => {
            // Bulk string: $5\r\nhello\r\n
            let len = get_decimal(cursor)?;
            if len == -1 {
                return Ok(()); // Null bulk
            }
            // Skip payload + \r\n
            let len = len as usize;
            if cursor.remaining() < len + 2 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            cursor.advance(len + 2);
            Ok(())
        }
        b'*' => {
            // Array: *2\r\n...
            let len = get_decimal(cursor)?;
            for _ in 0..len {
                check_frame(cursor)?;
            }
            Ok(())
        }
        b'a'..=b'z' | b'A'..=b'Z' => {
            // Inline command: plain text ending with \r\n
            // Only accept alphabetic characters as valid inline commands
            // Put the byte back and read until \r\n
            cursor.set_position(cursor.position() - 1);
            get_line(cursor)?;
            Ok(())
        }
        _ => {
            // Invalid type byte - this is corruption
            Err(anyhow!("Invalid RESP type byte: {:#x}", type_byte))
        }
    }
}

// Actually extracts the data
fn parse_frame<'a>(cursor: &mut io::Cursor<&'a [u8]>, src: &'a BytesMut) -> Result<Frame> {
    let type_byte = cursor.get_u8();

    match type_byte {
        b'+' => {
            let line = get_line_str(cursor, src)?.to_string();
            Ok(Frame::Simple(line))
        }
        b'-' => {
            let line = get_line_str(cursor, src)?.to_string();
            Ok(Frame::Error(line))
        }
        b':' => {
            let line = get_line_str(cursor, src)?;
            let num = line.parse::<i64>()?;
            Ok(Frame::Integer(num))
        }
        b'$' => {
            let len = get_decimal(cursor)?;
            if len == -1 {
                Ok(Frame::Null)
            } else {
                let len = len as usize;
                // Zero-copy slicing!
                // We map the cursor position to the actual buffer slice
                let start = cursor.position() as usize;
                let data = src[start..start + len].to_vec();
                cursor.advance(len + 2); // Skip data + \r\n
                Ok(Frame::Bulk(data.into()))
            }
        }
        b'*' => {
            let len = get_decimal(cursor)?;
            let mut out = Vec::with_capacity(len as usize);
            for _ in 0..len {
                out.push(parse_frame(cursor, src)?);
            }
            Ok(Frame::Array(out))
        }
        b'a'..=b'z' | b'A'..=b'Z' => {
            // Inline command: "PING\r\n" or "SET key value\r\n"
            // Only accept alphabetic characters as valid inline commands
            // Put the byte back
            cursor.set_position(cursor.position() - 1);
            let line = get_line_str(cursor, src)?;

            // Split by whitespace and convert to Frame::Array
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                return Err(anyhow!("Empty inline command"));
            }

            let frames: Vec<Frame> = parts
                .into_iter()
                .map(|s| Frame::Bulk(Bytes::from(s.to_string())))
                .collect();

            Ok(Frame::Array(frames))
        }
        _ => {
            // Invalid type byte - this is corruption
            Err(anyhow!("Invalid RESP type byte: {:#x}", type_byte))
        }
    }
}

// Utility to read until \r\n
fn get_line<'a>(cursor: &mut io::Cursor<&'a [u8]>) -> Result<&'a [u8]> {
    let start = cursor.position() as usize;
    let end = cursor.get_ref().len();

    for i in start..end - 1 {
        if cursor.get_ref()[i] == b'\r' && cursor.get_ref()[i + 1] == b'\n' {
            cursor.set_position((i + 2) as u64);
            return Ok(&cursor.get_ref()[start..i]);
        }
    }
    Err(io::Error::from(io::ErrorKind::UnexpectedEof).into())
}

// Utility to read a line and parse as ASCII string
fn get_line_str<'a>(cursor: &mut io::Cursor<&'a [u8]>, _src: &'a BytesMut) -> Result<&'a str> {
    // In parsing mode, we need to be careful.
    // The cursor is iterating over a slice of `src`.
    // However, for Simplicity in this MVP, we re-parse the line bounds.
    let line_bytes = get_line(cursor)?;
    std::str::from_utf8(line_bytes).map_err(|e| e.into())
}

fn get_decimal(cursor: &mut io::Cursor<&[u8]>) -> Result<i64> {
    let line = get_line(cursor)?;
    let s = std::str::from_utf8(line)?;
    Ok(s.parse::<i64>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{Bytes, BytesMut};

    #[test]
    fn test_decode_simple_string() {
        let mut codec = RespCodec;
        let mut buf = BytesMut::from("+OK\r\n");
        let res = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(res, Frame::Simple("OK".to_string()));
    }

    #[test]
    fn test_decode_bulk_string() {
        let mut codec = RespCodec;
        let mut buf = BytesMut::from("$6\r\nfoobar\r\n");
        let res = codec.decode(&mut buf).unwrap().unwrap();

        if let Frame::Bulk(b) = res {
            assert_eq!(b, "foobar");
        } else {
            panic!("Expected Bulk string");
        }
    }

    #[test]
    fn test_decode_array() {
        let mut codec = RespCodec;
        let mut buf = BytesMut::from("*2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n");
        let res = codec.decode(&mut buf).unwrap().unwrap();

        if let Frame::Array(frames) = res {
            assert_eq!(frames.len(), 2);
            assert_eq!(frames[0], Frame::Bulk(Bytes::from("foo")));
            assert_eq!(frames[1], Frame::Bulk(Bytes::from("bar")));
        } else {
            panic!("Expected Array");
        }
    }

    #[test]
    fn test_decode_partial_frame() {
        let mut codec = RespCodec;
        // Send half a command
        let mut buf = BytesMut::from("*2\r\n$3\r\nfoo\r\n");

        // Should return Ok(None) -> "I need more bytes"
        let res = codec.decode(&mut buf).unwrap();
        assert!(res.is_none());

        // Add the rest
        buf.extend_from_slice(b"$3\r\nbar\r\n");
        let res = codec.decode(&mut buf).unwrap().unwrap();

        if let Frame::Array(frames) = res {
            assert_eq!(frames.len(), 2);
        } else {
            panic!("Expected Array");
        }
    }
}
