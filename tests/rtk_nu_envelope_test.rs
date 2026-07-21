use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::Value;
use std::env;
use std::path::Path;
use std::process::Command;

fn invoke(format: &str, fixture_mode: &str) -> std::process::Output {
    let adapter = env!("CARGO_BIN_EXE_rtk_nu");
    let adapter_dir = Path::new(adapter)
        .parent()
        .expect("adapter binary parent directory");
    let mut path_entries = vec![adapter_dir.to_path_buf()];
    if let Some(existing_path) = env::var_os("PATH") {
        path_entries.extend(env::split_paths(&existing_path));
    }
    let path = env::join_paths(path_entries).expect("construct test PATH");

    Command::new(adapter)
        .env("PATH", path)
        .args(["--format", format, "--"])
        .arg(env!("CARGO_BIN_EXE_rtk_nu_test_fixture"))
        .arg(fixture_mode)
        .output()
        .expect("run rtk_nu")
}

fn invoke_with(format: &str, fixture_mode: &str, extra_flags: &[&str]) -> std::process::Output {
    let adapter = env!("CARGO_BIN_EXE_rtk_nu");
    Command::new(adapter)
        .args(["--format", format])
        .args(extra_flags)
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_rtk_nu_test_fixture"))
        .arg(fixture_mode)
        .output()
        .expect("run rtk_nu")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn captured_stream_bytes(envelope: &Value, stream: &str) -> Vec<u8> {
    envelope["frames"]
        .as_array()
        .expect("frames array")
        .iter()
        .filter(|frame| frame["stream"] == stream)
        .flat_map(|frame| {
            BASE64
                .decode(frame["payload_base64"].as_str().expect("base64 payload"))
                .expect("decode payload")
        })
        .collect()
}

#[test]
fn preserves_binary_bytes_stream_offsets_and_nonzero_exit() {
    let output = invoke("json", "binary-failure");
    assert_eq!(output.status.code(), Some(7));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(envelope["schema_version"], "flexnetos.rtk_nu.envelope.v1");
    assert_eq!(envelope["completion"]["exit"]["code"], 7);
    assert_eq!(captured_stream_bytes(&envelope, "stdout"), vec![0xff, b'A']);
    assert_eq!(captured_stream_bytes(&envelope, "stderr"), vec![0, b'B']);
    for frame in envelope["frames"].as_array().expect("frames") {
        let bytes = BASE64
            .decode(frame["payload_base64"].as_str().expect("base64"))
            .expect("decode");
        assert_eq!(frame["byte_length"].as_u64(), Some(bytes.len() as u64));
        assert!(frame["provisional_frame_id"]
            .as_str()
            .expect("frame id")
            .starts_with("provisional:frame:"));
    }
}

#[test]
fn jsonl_has_monotonic_frames_and_completion_after_partial_lines() {
    let output = invoke("jsonl", "partial");
    assert!(output.status.success());
    let records = std::str::from_utf8(&output.stdout)
        .expect("JSONL UTF-8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSONL object"))
        .collect::<Vec<_>>();
    assert!(records.len() >= 2, "at least one frame plus completion");
    let frames = records
        .iter()
        .filter(|record| record["event_type"] == "raw_frame")
        .collect::<Vec<_>>();
    let sequences = frames
        .iter()
        .map(|record| record["frame"]["sequence"].as_u64().expect("sequence"))
        .collect::<Vec<_>>();
    assert_eq!(sequences, (1..=sequences.len() as u64).collect::<Vec<_>>());
    let completion = records.last().expect("completion record");
    assert_eq!(completion["event_type"], "execution_complete");
    assert_eq!(completion["stdout_byte_length"], 9);
    assert_eq!(completion["stderr_byte_length"], 7);
}

#[cfg(unix)]
#[test]
fn signal_termination_reports_signal_and_preserves_frames() {
    let output = invoke("json", "signal-abort");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(envelope["completion"]["exit"]["code"], Value::Null);
    // std::process::abort raises SIGABRT (6).
    assert_eq!(envelope["completion"]["exit"]["signal"], 6);
    assert_eq!(envelope["completion"]["exit"]["success"], false);
    assert_eq!(
        captured_stream_bytes(&envelope, "stdout"),
        b"pre-signal-out".to_vec()
    );
    assert_eq!(
        captured_stream_bytes(&envelope, "stderr"),
        b"pre-signal-err".to_vec()
    );
}

#[test]
fn large_stream_chunking_preserves_order_offsets_and_digests() {
    let output = invoke("json", "large-stream");
    assert!(output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    let frames = envelope["frames"].as_array().expect("frames array");
    let stdout_frames = frames
        .iter()
        .filter(|frame| frame["stream"] == "stdout")
        .collect::<Vec<_>>();
    assert!(
        stdout_frames.len() >= 2,
        "a 64 KiB stream must chunk into multiple frames, got {}",
        stdout_frames.len()
    );

    let mut expected_offset = 0u64;
    let mut reassembled = Vec::new();
    for frame in &stdout_frames {
        assert_eq!(
            frame["byte_offset"].as_u64().expect("byte_offset"),
            expected_offset,
            "stdout frame offsets must be contiguous"
        );
        let bytes = BASE64
            .decode(frame["payload_base64"].as_str().expect("base64"))
            .expect("decode payload");
        assert_eq!(
            frame["sha256"].as_str().expect("frame digest"),
            sha256_hex(&bytes),
            "per-frame digest must match the exact payload bytes"
        );
        expected_offset += bytes.len() as u64;
        reassembled.extend_from_slice(&bytes);
    }
    let expected: Vec<u8> = (0..65_536u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(reassembled, expected, "reassembled bytes must be exact");
    assert_eq!(envelope["completion"]["stdout_byte_length"], 65_536);
    assert_eq!(
        captured_stream_bytes(&envelope, "stderr"),
        b"large-marker".to_vec()
    );

    let all_sequences = frames
        .iter()
        .map(|frame| frame["sequence"].as_u64().expect("sequence"))
        .collect::<Vec<_>>();
    let mut sorted = all_sequences.clone();
    sorted.sort_unstable();
    assert_eq!(all_sequences, sorted, "frame sequence must be monotonic");
}

#[test]
fn parser_declaration_survives_garbage_without_transforming_bytes() {
    let output = invoke_with(
        "json",
        "parser-garbage",
        &["--parser-name", "json", "--parser-revision", "1"],
    );
    assert!(output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(envelope["metadata"]["parser_name"], "json");
    assert_eq!(envelope["metadata"]["parser_revision"], "1");
    assert_eq!(envelope["metadata"]["parser_status"], "not_attempted");
    assert_eq!(envelope["metadata"]["parser_error"], Value::Null);
    assert_eq!(
        captured_stream_bytes(&envelope, "stdout"),
        b"{not json\xff\xfe".to_vec(),
        "unparseable bytes must be retained exactly; parsing belongs to the Nu boundary"
    );
}

#[test]
fn compact_view_linkage_supplements_but_never_replaces_raw_frames() {
    let output = invoke_with(
        "json",
        "compact",
        &["--rtk-filter", "gh", "--rtk-filter-revision", "0.43.0"],
    );
    assert!(output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(envelope["metadata"]["rtk_filter"], "gh");
    assert_eq!(envelope["metadata"]["rtk_filter_revision"], "0.43.0");
    // The adapter links the compact view by name/revision; it never replaces
    // raw frames with a transformed representation of its own.
    assert_eq!(envelope["metadata"]["compact_representation"], Value::Null);
    assert_eq!(
        captured_stream_bytes(&envelope, "stdout"),
        b"compact-source".to_vec()
    );
}

#[test]
fn nuon_output_uses_nuon_records_for_from_nuon_boundaries() {
    let output = invoke("nuon", "nuon");
    assert!(output.status.success());
    let rendered = std::str::from_utf8(&output.stdout).expect("Nuon UTF-8");
    assert!(rendered.starts_with("{schema_version:"));
    assert!(rendered.contains("parser_status: \"not_attempted\""));
    assert!(rendered.contains("payload_base64: \"bnVvbg==\""));
}
