//! Byte-first legacy command adapter for Nushell ingestion.
//!
//! `rtk_nu` is intentionally separate from `rtk`: it captures process bytes before any
//! filter, parser, truncation, or normalization step, then emits an envelope that Nushell
//! can convert into typed values.  It never invokes a Nushell plugin and it does not handle
//! an already-typed Nu pipeline.

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use clap::{Parser, ValueEnum};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: &str = "flexnetos.rtk_nu.envelope.v1";
const PARSER_STATUS_NOT_ATTEMPTED: &str = "not_attempted";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Jsonl,
    Json,
    Nuon,
}

#[derive(Debug, Parser)]
#[command(
    name = "rtk_nu",
    version,
    about = "Capture legacy command bytes and emit a lossless Nushell ingestion envelope"
)]
struct Cli {
    /// Envelope serialization. JSONL emits one frame per line; JSON and Nuon emit one aggregate.
    #[arg(long, value_enum, default_value_t = OutputFormat::Jsonl)]
    format: OutputFormat,

    /// Working directory for the executed command. Defaults to the current directory.
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// Database-issued identity fields. Absent values are explicitly marked provisional.
    #[arg(long)]
    tenant_id: Option<String>,
    #[arg(long)]
    identity_id: Option<String>,
    #[arg(long)]
    grant_id: Option<String>,
    #[arg(long)]
    lease_id: Option<String>,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long)]
    request_id: Option<String>,
    #[arg(long)]
    execution_id: Option<String>,
    #[arg(long)]
    task_id: Option<String>,
    #[arg(long)]
    branch_id: Option<String>,

    /// Digest of the selected execution environment. Raw environment values are never emitted.
    #[arg(long)]
    environment_digest: Option<String>,

    /// Idempotency key used by the downstream owner/client protocol.
    #[arg(long)]
    idempotency_key: Option<String>,

    /// The configured RTK filter name. This adapter does not transform raw process output.
    #[arg(long, default_value = "unconfigured")]
    rtk_filter: String,

    /// Revision of the configured RTK filter.
    #[arg(long, default_value = "unconfigured")]
    rtk_filter_revision: String,

    /// Parser declared for the next boundary; parsing is deliberately not performed here.
    #[arg(long, default_value = "nushell")]
    parser_name: String,

    /// Revision of the declared parser.
    #[arg(long, default_value = "0.113.1")]
    parser_revision: String,

    /// Provenance seed retained by the downstream witness chain.
    #[arg(long)]
    witness_seed: Option<String>,

    /// The legacy command to run. Use `--` before the command when it has flags.
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    command: Vec<OsString>,
}

#[derive(Clone, Serialize)]
struct IdentityScope {
    tenant_id: String,
    identity_id: String,
    grant_id: String,
    lease_id: String,
    session_id: String,
    request_id: String,
    execution_id: String,
    task_id: String,
    branch_id: String,
}

#[derive(Clone, Serialize)]
struct ExecutionMetadata {
    schema_version: &'static str,
    identity: IdentityScope,
    argv: Vec<String>,
    argv_bytes_base64: Vec<String>,
    cwd: String,
    selected_environment_digest: String,
    idempotency_key: String,
    rtk_filter: String,
    rtk_filter_revision: String,
    parser_name: String,
    parser_revision: String,
    parser_status: &'static str,
    parser_error: Option<String>,
    compact_representation: Option<String>,
    typed_payload: Option<serde_json::Value>,
    provenance_witness_seed: String,
    started_at_unix_ms: u128,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum StreamName {
    Stdout,
    Stderr,
}

#[derive(Clone, Serialize)]
struct RawFrame {
    sequence: u64,
    provisional_frame_id: String,
    provisional_content_id: String,
    canonical_raw_object_id: Option<String>,
    stream: StreamName,
    byte_offset: u64,
    byte_length: usize,
    payload_base64: String,
    sha256: String,
}

#[derive(Serialize)]
struct FrameEnvelope {
    event_type: &'static str,
    metadata: ExecutionMetadata,
    frame: RawFrame,
}

#[derive(Clone, Serialize)]
struct ExitDetails {
    code: Option<i32>,
    signal: Option<i32>,
    success: bool,
    launch_error: Option<String>,
}

#[derive(Serialize)]
struct CompletionEnvelope {
    event_type: &'static str,
    metadata: ExecutionMetadata,
    frame_count: usize,
    stdout_byte_length: u64,
    stderr_byte_length: u64,
    completed_at_unix_ms: u128,
    duration_ms: u128,
    exit: ExitDetails,
}

#[derive(Serialize)]
struct AggregateEnvelope {
    schema_version: &'static str,
    event_type: &'static str,
    metadata: ExecutionMetadata,
    frames: Vec<RawFrame>,
    completion: CompletionSummary,
}

#[derive(Serialize)]
struct CompletionSummary {
    frame_count: usize,
    stdout_byte_length: u64,
    stderr_byte_length: u64,
    completed_at_unix_ms: u128,
    duration_ms: u128,
    exit: ExitDetails,
}

struct CapturedChunk {
    stream: StreamName,
    bytes: Vec<u8>,
}

fn main() {
    let exit_code = match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("rtk_nu: {error:#}");
            2
        }
    };
    std::process::exit(exit_code);
}

fn run(cli: Cli) -> Result<i32> {
    let started = Instant::now();
    let started_at_unix_ms = unix_millis();
    let cwd = cli
        .cwd
        .unwrap_or(std::env::current_dir().context("determine current working directory")?);
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("resolve working directory {}", cwd.display()))?;
    let command_bytes = cli
        .command
        .iter()
        .map(|value| os_bytes(value.as_os_str()))
        .collect::<Vec<_>>();
    let seed = cli
        .witness_seed
        .unwrap_or_else(|| generated_seed(&command_bytes, &cwd));
    let execution_id = cli
        .execution_id
        .unwrap_or_else(|| provisional_id("execution", &seed));
    let identity = IdentityScope {
        tenant_id: resolve_id(cli.tenant_id, "tenant", &execution_id),
        identity_id: resolve_id(cli.identity_id, "identity", &execution_id),
        grant_id: resolve_id(cli.grant_id, "grant", &execution_id),
        lease_id: resolve_id(cli.lease_id, "lease", &execution_id),
        session_id: resolve_id(cli.session_id, "session", &execution_id),
        request_id: resolve_id(cli.request_id, "request", &execution_id),
        execution_id,
        task_id: resolve_id(cli.task_id, "task", &seed),
        branch_id: resolve_id(cli.branch_id, "branch", &seed),
    };
    let metadata = ExecutionMetadata {
        schema_version: SCHEMA_VERSION,
        identity,
        argv: cli
            .command
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect(),
        argv_bytes_base64: command_bytes
            .iter()
            .map(|bytes| BASE64.encode(bytes))
            .collect(),
        cwd: cwd.display().to_string(),
        selected_environment_digest: cli
            .environment_digest
            .unwrap_or_else(|| "unprovided".to_string()),
        idempotency_key: cli.idempotency_key.unwrap_or_else(|| {
            format!(
                "rtk_nu:{}",
                sha256_hex(format!("{}:{}", seed, started_at_unix_ms).as_bytes())
            )
        }),
        rtk_filter: cli.rtk_filter,
        rtk_filter_revision: cli.rtk_filter_revision,
        parser_name: cli.parser_name,
        parser_revision: cli.parser_revision,
        parser_status: PARSER_STATUS_NOT_ATTEMPTED,
        parser_error: None,
        compact_representation: None,
        typed_payload: None,
        provenance_witness_seed: seed,
        started_at_unix_ms,
    };

    let (sender, receiver) = mpsc::channel();
    let mut command = Command::new(&cli.command[0]);
    command
        .args(&cli.command[1..])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let exit = ExitDetails {
                code: None,
                signal: None,
                success: false,
                launch_error: Some(error.to_string()),
            };
            emit_without_frames(cli.format, metadata, started, exit)?;
            return Ok(127);
        }
    };
    let stdout = child.stdout.take().context("capture child stdout")?;
    let stderr = child.stderr.take().context("capture child stderr")?;
    let stdout_thread = spawn_reader(StreamName::Stdout, stdout, sender.clone());
    let stderr_thread = spawn_reader(StreamName::Stderr, stderr, sender);

    let mut frames = Vec::new();
    let mut sequence = 0_u64;
    let mut stdout_offset = 0_u64;
    let mut stderr_offset = 0_u64;
    drain_frames(
        &receiver,
        &metadata,
        cli.format,
        &mut frames,
        &mut sequence,
        &mut stdout_offset,
        &mut stderr_offset,
    )?;
    join_reader(stdout_thread, "stdout")?;
    join_reader(stderr_thread, "stderr")?;
    let status = child.wait().context("wait for child process")?;
    let exit = exit_details(status);
    let completed_at_unix_ms = unix_millis();
    let completion = CompletionSummary {
        frame_count: frames.len(),
        stdout_byte_length: stdout_offset,
        stderr_byte_length: stderr_offset,
        completed_at_unix_ms,
        duration_ms: started.elapsed().as_millis(),
        exit: exit.clone(),
    };
    emit_completion(cli.format, metadata, frames, completion)?;
    Ok(exit.code.unwrap_or(1))
}

fn emit_without_frames(
    format: OutputFormat,
    metadata: ExecutionMetadata,
    started: Instant,
    exit: ExitDetails,
) -> Result<()> {
    let completion = CompletionSummary {
        frame_count: 0,
        stdout_byte_length: 0,
        stderr_byte_length: 0,
        completed_at_unix_ms: unix_millis(),
        duration_ms: started.elapsed().as_millis(),
        exit,
    };
    emit_completion(format, metadata, Vec::new(), completion)
}

fn spawn_reader<R: Read + Send + 'static>(
    stream: StreamName,
    reader: R,
    sender: Sender<CapturedChunk>,
) -> thread::JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        let mut reader = reader;
        let mut buffer = [0_u8; 8192];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            if sender
                .send(CapturedChunk {
                    stream,
                    bytes: buffer[..count].to_vec(),
                })
                .is_err()
            {
                return Ok(());
            }
        }
    })
}

fn drain_frames(
    receiver: &Receiver<CapturedChunk>,
    metadata: &ExecutionMetadata,
    format: OutputFormat,
    frames: &mut Vec<RawFrame>,
    sequence: &mut u64,
    stdout_offset: &mut u64,
    stderr_offset: &mut u64,
) -> Result<()> {
    let mut writer = io::BufWriter::new(io::stdout().lock());
    while let Ok(chunk) = receiver.recv() {
        *sequence += 1;
        let offset = match chunk.stream {
            StreamName::Stdout => {
                let offset = *stdout_offset;
                *stdout_offset += chunk.bytes.len() as u64;
                offset
            }
            StreamName::Stderr => {
                let offset = *stderr_offset;
                *stderr_offset += chunk.bytes.len() as u64;
                offset
            }
        };
        let digest = sha256_hex(&chunk.bytes);
        let frame = RawFrame {
            sequence: *sequence,
            provisional_frame_id: provisional_id(
                "frame",
                &format!("{}:{}:{}", metadata.identity.execution_id, sequence, digest),
            ),
            provisional_content_id: format!("sha256:{digest}"),
            canonical_raw_object_id: None,
            stream: chunk.stream,
            byte_offset: offset,
            byte_length: chunk.bytes.len(),
            payload_base64: BASE64.encode(&chunk.bytes),
            sha256: digest,
        };
        if matches!(format, OutputFormat::Jsonl) {
            serde_json::to_writer(
                &mut writer,
                &FrameEnvelope {
                    event_type: "raw_frame",
                    metadata: metadata.clone(),
                    frame: frame.clone(),
                },
            )
            .context("serialize JSONL raw frame")?;
            writer.write_all(b"\n").context("write JSONL raw frame")?;
            writer.flush().context("flush JSONL raw frame")?;
        }
        frames.push(frame);
    }
    Ok(())
}

fn join_reader(handle: thread::JoinHandle<io::Result<()>>, stream: &str) -> Result<()> {
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("{stream} capture thread panicked"))?
        .with_context(|| format!("read {stream} stream"))
}

fn emit_completion(
    format: OutputFormat,
    metadata: ExecutionMetadata,
    frames: Vec<RawFrame>,
    completion: CompletionSummary,
) -> Result<()> {
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());
    match format {
        OutputFormat::Jsonl => {
            serde_json::to_writer(
                &mut writer,
                &CompletionEnvelope {
                    event_type: "execution_complete",
                    metadata,
                    frame_count: completion.frame_count,
                    stdout_byte_length: completion.stdout_byte_length,
                    stderr_byte_length: completion.stderr_byte_length,
                    completed_at_unix_ms: completion.completed_at_unix_ms,
                    duration_ms: completion.duration_ms,
                    exit: completion.exit,
                },
            )
            .context("serialize JSONL completion")?;
            writer.write_all(b"\n").context("write JSONL completion")?;
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(
                &mut writer,
                &AggregateEnvelope {
                    schema_version: SCHEMA_VERSION,
                    event_type: "execution",
                    metadata,
                    frames,
                    completion,
                },
            )
            .context("serialize JSON envelope")?;
            writer.write_all(b"\n").context("write JSON envelope")?;
        }
        OutputFormat::Nuon => {
            let value = serde_json::to_value(AggregateEnvelope {
                schema_version: SCHEMA_VERSION,
                event_type: "execution",
                metadata,
                frames,
                completion,
            })
            .context("convert envelope for Nuon")?;
            writer
                .write_all(json_compatible_nuon(&value).as_bytes())
                .context("write Nuon envelope")?;
            writer.write_all(b"\n").context("terminate Nuon envelope")?;
        }
    }
    writer.flush().context("flush envelope")
}

/// Render the JSON data model emitted by this adapter using Nuon's record syntax. Base64 payloads
/// remain JSON-quoted strings, while records use Nuon field names instead of JSON-quoted keys.
/// This is a serialization boundary only; it never decodes or transforms captured bytes.
fn json_compatible_nuon(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => {
            serde_json::to_string(value).expect("serialize Nuon string")
        }
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(json_compatible_nuon)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        serde_json::Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    nuon_field_name(key),
                    json_compatible_nuon(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn nuon_field_name(name: &str) -> String {
    if name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        name.to_string()
    } else {
        serde_json::to_string(name).expect("serialize Nuon record field")
    }
}

fn exit_details(status: ExitStatus) -> ExitDetails {
    ExitDetails {
        code: status.code(),
        signal: signal_number(&status),
        success: status.success(),
        launch_error: None,
    }
}

#[cfg(unix)]
fn signal_number(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_number(_status: &ExitStatus) -> Option<i32> {
    None
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn generated_seed(argv: &[Vec<u8>], cwd: &Path) -> String {
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut material = format!("{}:{}:{}", unix_millis(), std::process::id(), counter).into_bytes();
    material.extend_from_slice(cwd.as_os_str().to_string_lossy().as_bytes());
    for arg in argv {
        material.extend_from_slice(arg);
    }
    format!("sha256:{}", sha256_hex(&material))
}

fn provisional_id(kind: &str, material: &str) -> String {
    format!(
        "provisional:{kind}:{}",
        &sha256_hex(material.as_bytes())[..24]
    )
}

fn resolve_id(value: Option<String>, kind: &str, material: &str) -> String {
    value.unwrap_or_else(|| provisional_id(kind, material))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_bytes_are_base64_lossless_and_digest_addressed() {
        let bytes = vec![0, 0xff, b'a', b'\n'];
        let encoded = BASE64.encode(&bytes);
        assert_eq!(BASE64.decode(encoded).expect("decode raw bytes"), bytes);
        assert_eq!(
            sha256_hex(&bytes),
            "ab537004c8945f156a7ad256f1458647062a1b9d66c60ee78101daef9b58b01d"
        );
    }

    #[test]
    fn provisional_ids_are_stable_for_same_material() {
        assert_eq!(
            provisional_id("frame", "execution:1:digest"),
            provisional_id("frame", "execution:1:digest")
        );
    }

    #[test]
    fn nuon_serializer_uses_unquoted_record_fields_and_quoted_strings() {
        let value = serde_json::json!({
            "schema_version": "v1",
            "frames": [{ "stream": "stdout", "canonical_raw_object_id": null }],
        });
        assert_eq!(
            json_compatible_nuon(&value),
            "{schema_version: \"v1\", frames: [{stream: \"stdout\", canonical_raw_object_id: null}]}"
        );
    }
}
