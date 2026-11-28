//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use axum::http::Response;
use std::fmt::Write;

fn stream(body: String) -> SQLStream {
    SQLStream::new(Response::new(body).into(), "request".into())
}

#[test]
fn a_chunk_with_thousands_of_rows_uses_linear_allocation_volume() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let measure = |rows: usize| {
        let mut body = format!("{{\"type\":\"metadata\",\"columns\":[],\"row_count\":{rows},\"spilled_to_disk\":false,\"request_id\":\"request\"}}\n");
        for _ in 0..rows {
            body.push_str("{\"type\":\"row\",\"row\":{}}\n");
        }
        writeln!(
            body,
            r#"{{"type":"complete","row_count":{rows},"request_id":"request"}}"#
        )
        .unwrap();
        let mut stream = stream(body);
        allocation_counter::measure(|| runtime.block_on(async {
            assert!(matches!(stream.next_frame().await.unwrap(), Some(SQLStreamFrame::Metadata { row_count, .. }) if row_count == rows));
            for _ in 0..rows {
                assert!(matches!(stream.next_frame().await.unwrap(), Some(SQLStreamFrame::Row { row }) if row.is_empty()));
            }
            assert!(matches!(stream.next_frame().await.unwrap(), Some(SQLStreamFrame::Complete { row_count, .. }) if row_count == rows));
            assert!(stream.next_frame().await.unwrap().is_none());
        })).bytes_total
    };
    let first = measure(2048);
    let second = measure(4096);
    assert!(
        second <= 2 * first + 4096,
        "2048 rows allocated {first} bytes, 4096 allocated {second}"
    );
}

#[test]
fn scanning_partial_frames_preserves_delimiters_and_the_per_frame_limit() {
    let wire = b"12345678\n\n\r\nab\nunfinished";
    for chunk_size in 1..=wire.len() {
        let mut lines = FrameLines::default();
        let mut complete = Vec::new();
        for chunk in wire.chunks(chunk_size) {
            lines.append(chunk);
            while let Some(line) = lines.next_line(10).unwrap() {
                complete.push(line.to_vec());
            }
        }
        assert_eq!(complete, [&b"12345678"[..], b"", b"\r", b"ab"]);
        assert_eq!(lines.take_remaining(), b"unfinished");
        assert!(lines.remaining().is_empty());
    }
    for suffix in [b"toolong".as_slice(), b"toolong\n"] {
        let mut lines = FrameLines::default();
        lines.append(b"short\n");
        assert_eq!(lines.next_line(5).unwrap(), Some(b"short".as_slice()));
        lines.append(suffix);
        assert!(matches!(
            lines.next_line(5),
            Err(HttpEngineError::StreamFrameTooLarge)
        ));
        assert_eq!(lines.remaining(), suffix);
    }
}

#[tokio::test]
async fn eof_after_metadata_and_terminal_frames_keeps_its_decode_state() {
    let metadata = r#"{"type":"metadata","columns":[],"row_count":0,"spilled_to_disk":false,"request_id":"request"}"#;
    let complete = r#"{"type":"complete","row_count":0,"request_id":"request"}"#;
    for suffix in ["", "\n", "\n\r", "\r\n\n\r\n"] {
        let mut frames = stream(format!("\r\n{metadata}\r\n{complete}{suffix}"));
        assert!(matches!(
            frames.next_frame().await.unwrap(),
            Some(SQLStreamFrame::Metadata { .. })
        ));
        assert!(matches!(
            frames.next_frame().await.unwrap(),
            Some(SQLStreamFrame::Complete { .. })
        ));
        assert!(frames.next_frame().await.unwrap().is_none());
        assert!(frames.next_frame().await.unwrap().is_none());
    }
    let mut invalid = stream(format!("{metadata}\n{complete}\ntrailing"));
    invalid.next_frame().await.unwrap();
    invalid.next_frame().await.unwrap();
    for _ in 0..2 {
        assert!(matches!(
            invalid.next_frame().await,
            Err(HttpEngineError::InvalidStreamSequence)
        ));
    }
    let mut unfinished = stream(format!("{metadata}\n\r"));
    unfinished.next_frame().await.unwrap();
    assert!(matches!(
        unfinished.next_frame().await,
        Err(HttpEngineError::InvalidResponse(_))
    ));
    assert!(matches!(
        unfinished.next_frame().await,
        Err(HttpEngineError::TruncatedStream)
    ));
}
