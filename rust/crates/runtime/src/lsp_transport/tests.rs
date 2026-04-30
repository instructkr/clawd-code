use super::*;
use std::io::Cursor;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

#[test]
fn content_length_header_roundtrip() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let payload = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":null}"#;

        // Write frame into a buffer
        let mut write_buf = Vec::new();
        {
            let header = format!("Content-Length: {}\r\n\r\n", payload.len());
            write_buf.extend_from_slice(header.as_bytes());
            write_buf.extend_from_slice(payload);
        }

        // Read frame back using the same logic as LspTransport::read_frame
        let cursor = Cursor::new(write_buf);
        let mut reader = BufReader::new(cursor);

        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let bytes_read = reader.read_line(&mut line).await.unwrap();
            assert!(bytes_read > 0, "unexpected EOF reading header");
            if line == "\r\n" {
                break;
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if let Some((name, value)) = header.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    content_length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
        }

        let content_length = content_length.expect("should have Content-Length");
        assert_eq!(content_length, payload.len());

        let mut read_payload = vec![0u8; content_length];
        reader.read_exact(&mut read_payload).await.unwrap();

        let original: serde_json::Value = serde_json::from_slice(payload).unwrap();
        let roundtripped: serde_json::Value = serde_json::from_slice(&read_payload).unwrap();
        assert_eq!(original, roundtripped);
    });
}

#[test]
fn request_has_incrementing_ids() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // Spawn cat so we can construct a real LspTransport.
        let child = tokio::process::Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("cat should be available");

        let mut transport = LspTransport::from_child(child, Duration::from_secs(5));

        // Allocate IDs by inspecting what send_request would produce.
        let id1 = transport.allocate_id();
        let id2 = transport.allocate_id();
        let id3 = transport.allocate_id();

        assert_eq!(id1, LspId::Number(1));
        assert_eq!(id2, LspId::Number(2));
        assert_eq!(id3, LspId::Number(3));

        // Clean up
        let _ = transport.shutdown().await;
    });
}

#[test]
fn notification_has_no_id() {
    let notification = LspNotification::new("initialized", Some(serde_json::json!({})));
    let serialized = serde_json::to_string(&notification).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert!(
        parsed.get("id").is_none(),
        "notification should not contain an 'id' field, got: {serialized}"
    );
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["method"], "initialized");
}

#[test]
fn malformed_header_handling() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // Feed garbage bytes that don't contain a valid Content-Length header.
        let garbage = b"THIS IS NOT A VALID HEADER\r\n\r\n";
        let cursor = Cursor::new(garbage.to_vec());
        let mut reader = BufReader::new(cursor);

        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let bytes_read = reader.read_line(&mut line).await.unwrap();
            if bytes_read == 0 || line == "\r\n" {
                break;
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if let Some((name, value)) = header.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    content_length = value.trim().parse::<usize>().ok();
                }
            }
        }

        // The garbage header should not produce a valid Content-Length.
        assert!(
            content_length.is_none(),
            "garbage input should not produce a valid Content-Length"
        );
    });
}
