use std::io::{self, Read, Write};

use serde_json::{Value, json};

pub(super) const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_NATIVE_FRAME: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 128 * 1024;
// JavaScript slices strings by UTF-16 code units rather than UTF-8 bytes.
const MAX_INCOMING_CHUNK: usize = 4 * CHUNK_BYTES;
const MAX_CHUNKS: usize = 1024;

pub(super) fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid extension bridge message.",
    )
}

pub(super) fn encode(value: &Value) -> io::Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_MESSAGE - self.0.len() {
                return Err(invalid());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| invalid())?;
    Ok(output.0)
}

pub(super) fn read_frame(reader: &mut impl Read, limit: usize) -> io::Result<Value> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > limit {
        return Err(invalid());
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

pub(super) fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE {
        return Err(invalid());
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()
}

pub(super) fn request_id(value: &Value) -> io::Result<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or_else(invalid)
}

pub(super) fn validate_reply<'a>(value: &'a Value, id: &str) -> io::Result<&'a Value> {
    if request_id(value)? != id || value.get("type").is_some() {
        return Err(invalid());
    }
    match (value.get("result"), value.get("error")) {
        (Some(result), None) => Ok(result),
        (None, Some(Value::String(_))) => Ok(value),
        _ => Err(invalid()),
    }
}

pub(super) fn send_native(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let id = request_id(value)?;
    let bytes = encode(value)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    for (index, data) in chunks.iter().enumerate() {
        let chunk =
            json!({"type": "chunk", "id": id, "index": index, "total": chunks.len(), "data": data});
        let encoded = encode(&chunk)?;
        if encoded.len() > MAX_NATIVE_FRAME {
            return Err(invalid());
        }
        write_frame(writer, &encoded)?;
    }
    Ok(())
}

pub(super) fn receive_native(reader: &mut impl Read, id: &str) -> io::Result<Value> {
    let mut text = String::new();
    let mut total = None;
    for index in 0..MAX_CHUNKS {
        let value = read_frame(reader, MAX_NATIVE_FRAME)?;
        let count = value["total"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(invalid)?;
        let data = value["data"].as_str().ok_or_else(invalid)?;
        if value["type"] != "chunk"
            || request_id(&value)? != id
            || value["index"].as_u64() != Some(index as u64)
            || count == 0
            || count > MAX_CHUNKS
            || total.is_some_and(|expected| count != expected)
            || data.is_empty()
            || data.len() > MAX_INCOMING_CHUNK
            || data.len() > MAX_MESSAGE - text.len()
        {
            return Err(invalid());
        }
        total = Some(count);
        text.push_str(data);
        if index + 1 == count {
            let reply = serde_json::from_str(&text).map_err(|_| invalid())?;
            validate_reply(&reply, id)?;
            return Ok(reply);
        }
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn native_large_unicode_reply_round_trips_in_bounded_frames() {
        let reply = json!({"id": "test", "result": "📷\\\"".repeat(100_000)});
        let mut bytes = Vec::new();
        send_native(&mut bytes, &reply).unwrap();
        assert_eq!(
            receive_native(&mut Cursor::new(&bytes), "test").unwrap(),
            reply
        );
        let mut cursor = Cursor::new(&bytes);
        let mut count = 0;
        while cursor.position() < bytes.len() as u64 {
            let frame = read_frame(&mut cursor, MAX_NATIVE_FRAME).unwrap();
            assert_eq!(frame["index"], count);
            assert!(frame["data"].as_str().unwrap().len() <= CHUNK_BYTES);
            count += 1;
        }
        assert!(count > 1);
    }

    #[test]
    fn rejects_invalid_lengths_and_truncation() {
        for length in [0, MAX_MESSAGE + 1, u32::MAX as usize] {
            assert!(
                read_frame(&mut Cursor::new((length as u32).to_le_bytes()), MAX_MESSAGE).is_err()
            );
        }
        assert!(read_frame(&mut Cursor::new([2, 0, 0, 0, b'{']), MAX_MESSAGE).is_err());
    }

    #[test]
    fn rejects_wrong_ids_duplicate_indices_and_changing_totals() {
        let reply = json!({"id": "a", "result": "x".repeat(CHUNK_BYTES * 2)});
        let mut bytes = Vec::new();
        send_native(&mut bytes, &reply).unwrap();
        assert!(receive_native(&mut Cursor::new(&bytes), "b").is_err());
        let mut source = Cursor::new(&bytes);
        let first = read_frame(&mut source, MAX_NATIVE_FRAME).unwrap();
        let second = read_frame(&mut source, MAX_NATIVE_FRAME).unwrap();
        for field in ["id", "index", "total"] {
            let mut changed = second.clone();
            changed[field] = match field {
                "id" => json!("b"),
                "index" => json!(0),
                _ => json!(999),
            };
            let mut frames = Vec::new();
            write_frame(&mut frames, &encode(&first).unwrap()).unwrap();
            write_frame(&mut frames, &encode(&changed).unwrap()).unwrap();
            assert!(receive_native(&mut Cursor::new(frames), "a").is_err());
        }
    }

    #[test]
    fn rejects_oversized_assembled_messages_and_encoding() {
        assert!(encode(&json!({"id": "a", "command": "x".repeat(MAX_MESSAGE)})).is_err());
        let mut frames = Vec::new();
        for index in 0..=128 {
            let chunk = json!({"type": "chunk", "id": "a", "index": index, "total": 129, "data": "x".repeat(CHUNK_BYTES)});
            write_frame(&mut frames, &encode(&chunk).unwrap()).unwrap();
        }
        assert!(receive_native(&mut Cursor::new(frames), "a").is_err());
    }

    #[test]
    fn rejects_ambiguous_or_mismatched_replies() {
        for value in [
            json!({"id": "a"}),
            json!({"id": "a", "result": null, "error": "bad"}),
            json!({"id": "b", "result": null}),
            json!({"id": "a", "error": {}}),
        ] {
            assert!(validate_reply(&value, "a").is_err());
        }
        assert_eq!(
            validate_reply(&json!({"id": "a", "result": null}), "a").unwrap(),
            &Value::Null
        );
    }
}
