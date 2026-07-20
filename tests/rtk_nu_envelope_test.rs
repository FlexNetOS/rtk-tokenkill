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

#[test]
fn nuon_output_uses_nuon_records_for_from_nuon_boundaries() {
    let output = invoke("nuon", "nuon");
    assert!(output.status.success());
    let rendered = std::str::from_utf8(&output.stdout).expect("Nuon UTF-8");
    assert!(rendered.starts_with("{schema_version:"));
    assert!(rendered.contains("parser_status: \"not_attempted\""));
    assert!(rendered.contains("payload_base64: \"bnVvbg==\""));
}
