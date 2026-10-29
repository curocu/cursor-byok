//! Encodes AWS event-stream frames (`application/vnd.amazon.eventstream`).
//!
//! Kiro's bundled CodeWhisperer streaming client parses exactly this layout for
//! the runtime response, so the gateway must emit it byte for byte:
//!
//! ```text
//! [totalLength u32][headersLength u32][preludeCRC u32][headers...][payload...][messageCRC u32]
//! header: [nameLen u8][name][valueType u8 = 7 string][valueLen u16][value]
//! ```
//!
//! All integers are big-endian. `preludeCRC` covers the first 8 bytes;
//! `messageCRC` covers everything before itself.

use serde_json::Value;

/// Content type Kiro expects on the runtime response.
pub const CONTENT_TYPE: &str = "application/vnd.amazon.eventstream";

const HEADER_TYPE_STRING: u8 = 7;
const PRELUDE_LEN: usize = 12;
const TRAILER_LEN: usize = 4;

/// Encodes one `event` message carrying a JSON payload.
pub fn event(event_type: &str, payload: &Value) -> Vec<u8> {
    let headers = [
        header(":message-type", "event"),
        header(":event-type", event_type),
        header(":content-type", "application/json"),
    ]
    .concat();
    frame(&headers, payload.to_string().as_bytes())
}

/// Encodes one `exception` message, which is how the gateway surfaces a failure
/// to Kiro without dropping the stream.
pub fn exception(exception_type: &str, payload: &Value) -> Vec<u8> {
    let headers = [
        header(":message-type", "exception"),
        header(":exception-type", exception_type),
        header(":content-type", "application/json"),
    ]
    .concat();
    frame(&headers, payload.to_string().as_bytes())
}

fn header(name: &str, value: &str) -> Vec<u8> {
    let name = name.as_bytes();
    let value = value.as_bytes();
    let mut buffer = Vec::with_capacity(1 + name.len() + 1 + 2 + value.len());
    buffer.push(name.len() as u8);
    buffer.extend_from_slice(name);
    buffer.push(HEADER_TYPE_STRING);
    buffer.extend_from_slice(&(value.len() as u16).to_be_bytes());
    buffer.extend_from_slice(value);
    buffer
}

fn frame(headers: &[u8], payload: &[u8]) -> Vec<u8> {
    let total = PRELUDE_LEN + headers.len() + payload.len() + TRAILER_LEN;
    let mut buffer = Vec::with_capacity(total);
    buffer.extend_from_slice(&(total as u32).to_be_bytes());
    buffer.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32fast::hash(&buffer[..8]);
    buffer.extend_from_slice(&prelude_crc.to_be_bytes());
    buffer.extend_from_slice(headers);
    buffer.extend_from_slice(payload);
    let message_crc = crc32fast::hash(&buffer);
    buffer.extend_from_slice(&message_crc.to_be_bytes());
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Reads back a frame exactly the way Kiro's client does.
    struct Decoded {
        headers: Vec<(String, String)>,
        payload: Value,
    }

    fn decode(frame: &[u8]) -> Decoded {
        let total = u32::from_be_bytes(frame[0..4].try_into().unwrap()) as usize;
        let headers_len = u32::from_be_bytes(frame[4..8].try_into().unwrap()) as usize;
        let prelude_crc = u32::from_be_bytes(frame[8..12].try_into().unwrap());
        assert_eq!(total, frame.len(), "totalLength must cover the whole frame");
        assert_eq!(prelude_crc, crc32fast::hash(&frame[..8]));
        let message_crc = u32::from_be_bytes(frame[total - 4..total].try_into().unwrap());
        assert_eq!(message_crc, crc32fast::hash(&frame[..total - 4]));

        let mut headers = Vec::new();
        let mut cursor = PRELUDE_LEN;
        let end = PRELUDE_LEN + headers_len;
        while cursor < end {
            let name_len = frame[cursor] as usize;
            cursor += 1;
            let name = std::str::from_utf8(&frame[cursor..cursor + name_len]).unwrap();
            cursor += name_len;
            assert_eq!(frame[cursor], HEADER_TYPE_STRING);
            cursor += 1;
            let value_len = u16::from_be_bytes(frame[cursor..cursor + 2].try_into().unwrap()) as usize;
            cursor += 2;
            let value = std::str::from_utf8(&frame[cursor..cursor + value_len]).unwrap();
            cursor += value_len;
            headers.push((name.to_owned(), value.to_owned()));
        }
        assert_eq!(cursor, end);

        let payload = serde_json::from_slice(&frame[end..total - TRAILER_LEN]).unwrap();
        Decoded { headers, payload }
    }

    fn header_value<'a>(decoded: &'a Decoded, name: &str) -> &'a str {
        decoded
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("missing header {name}"))
    }

    #[test]
    fn an_event_frame_carries_the_event_type_header_and_json_payload() {
        let decoded = decode(&event(
            "assistantResponseEvent",
            &json!({"content": "hi", "modelId": "capture-probe"}),
        ));

        assert_eq!(header_value(&decoded, ":message-type"), "event");
        assert_eq!(header_value(&decoded, ":event-type"), "assistantResponseEvent");
        assert_eq!(header_value(&decoded, ":content-type"), "application/json");
        assert_eq!(decoded.payload["content"], json!("hi"));
    }

    #[test]
    fn an_exception_frame_is_distinguishable_from_an_event_frame() {
        let decoded = decode(&exception(
            "InternalServerException",
            &json!({"message": "upstream failed"}),
        ));

        assert_eq!(header_value(&decoded, ":message-type"), "exception");
        assert_eq!(
            header_value(&decoded, ":exception-type"),
            "InternalServerException"
        );
    }

    #[test]
    fn an_empty_payload_still_produces_a_well_formed_frame() {
        let decoded = decode(&event("contextUsageEvent", &json!({})));
        assert_eq!(header_value(&decoded, ":event-type"), "contextUsageEvent");
        assert_eq!(decoded.payload, json!({}));
    }
}
