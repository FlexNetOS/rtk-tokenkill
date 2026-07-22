//! Authenticated loopback HTTP server for RTK observability data.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::observability::{ApiEnvelope, HealthPayload};

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    reason: &'static str,
    body: Vec<u8>,
    extra_headers: Vec<(&'static str, &'static str)>,
}

#[derive(Serialize)]
struct ErrorPayload<'a> {
    error: &'a str,
}

pub fn run(bind: &str, icm_url: Option<&str>) -> Result<()> {
    let address = parse_loopback_bind(bind)?;
    let token = server_token_from(std::env::var("RTK_SERVER_TOKEN").ok())?;
    let normalized_icm = icm_url
        .map(crate::icm_bridge::normalize_loopback_url)
        .transpose()?;
    let listener = TcpListener::bind(address)
        .with_context(|| format!("Failed to bind RTK server to {address}"))?;
    println!("RTK server listening on http://{}", listener.local_addr()?);
    println!("All /v1 endpoints require RTK_SERVER_TOKEN; /health is public.");
    serve(listener, &token, normalized_icm.as_deref(), None)
}

fn parse_loopback_bind(value: &str) -> Result<SocketAddr> {
    let address = value
        .parse::<SocketAddr>()
        .with_context(|| format!("Invalid server bind address: {value}"))?;
    if !address.ip().is_loopback() {
        anyhow::bail!("RTK server only binds to loopback addresses");
    }
    Ok(address)
}

fn server_token_from(token: Option<String>) -> Result<String> {
    let token = token.context("RTK_SERVER_TOKEN is required")?;
    if token.trim().is_empty() {
        anyhow::bail!("RTK_SERVER_TOKEN must not be empty");
    }
    Ok(token)
}

fn serve(
    listener: TcpListener,
    token: &str,
    icm_url: Option<&str>,
    max_connections: Option<usize>,
) -> Result<()> {
    let mut served = 0_usize;
    loop {
        if max_connections.is_some_and(|maximum| served >= maximum) {
            return Ok(());
        }
        let (stream, _) = listener.accept().context("RTK server accept failed")?;
        served += 1;
        if let Err(error) = handle_connection(stream, token, icm_url) {
            eprintln!("rtk server: connection failed: {error:#}");
        }
    }
}

fn handle_connection(mut stream: TcpStream, token: &str, icm_url: Option<&str>) -> Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let response = match read_request(&mut stream).and_then(|raw| parse_request(&raw)) {
        Ok(request) => route_request(&request, token, icm_url),
        Err(_) => error_response(400, "Bad Request", "invalid HTTP request"),
    };
    write_response(&mut stream, &response)
}

fn read_request(stream: &mut TcpStream) -> Result<Vec<u8>> {
    read_request_with_timeout(stream, IO_TIMEOUT)
}

fn read_request_with_timeout(stream: &mut TcpStream, timeout: Duration) -> Result<Vec<u8>> {
    let started = Instant::now();
    let mut request = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    loop {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .context("HTTP request header deadline exceeded")?;
        stream.set_read_timeout(Some(remaining))?;
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > MAX_REQUEST_BYTES {
            anyhow::bail!("HTTP request headers exceed the size limit");
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(request);
        }
    }
    anyhow::bail!("incomplete HTTP request")
}

fn parse_request(raw: &[u8]) -> Result<HttpRequest> {
    let text = std::str::from_utf8(raw).context("request headers are not UTF-8")?;
    let header_end = text.find("\r\n\r\n").context("request has no header end")?;
    let mut lines = text[..header_end].split("\r\n");
    let request_line = lines.next().context("request line is missing")?;
    let parts = request_line.split_whitespace().collect::<Vec<_>>();
    let [method, target, version] = parts.as_slice() else {
        anyhow::bail!("invalid request line");
    };
    if !matches!(*version, "HTTP/1.0" | "HTTP/1.1") || !target.starts_with('/') {
        anyhow::bail!("unsupported HTTP request line");
    }
    let mut headers = HashMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').context("invalid HTTP header")?;
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || headers.insert(name, value.trim().to_string()).is_some() {
            anyhow::bail!("duplicate or empty HTTP header");
        }
    }
    let path = target.split('?').next().unwrap_or(target).to_string();
    Ok(HttpRequest {
        method: (*method).to_string(),
        path,
        headers,
    })
}

fn route_request(request: &HttpRequest, token: &str, icm_url: Option<&str>) -> HttpResponse {
    if request.path != "/health" && !request_is_authorized(request, token) {
        let mut response = error_response(401, "Unauthorized", "bearer token required");
        response
            .extra_headers
            .push(("WWW-Authenticate", "Bearer realm=\"rtk\""));
        return response;
    }
    if request.method != "GET" {
        return error_response(405, "Method Not Allowed", "only GET is supported");
    }

    let body = match request.path.as_str() {
        "/health" => serialize_payload(&HealthPayload::healthy()),
        "/v1/hooks" => crate::observability::collect_agents().and_then(|agents| {
            serialize_payload(&ApiEnvelope::new(crate::observability::hooks_from_agents(
                &agents,
            )))
        }),
        "/v1/agents" => crate::observability::collect_agents()
            .and_then(|agents| serialize_payload(&ApiEnvelope::new(agents))),
        "/v1/gain" => crate::observability::collect_gain()
            .and_then(|gain| serialize_payload(&ApiEnvelope::new(gain))),
        "/v1/failures" => crate::observability::collect_failures()
            .and_then(|failures| serialize_payload(&ApiEnvelope::new(failures))),
        "/v1/audit" => crate::observability::collect_audit(100)
            .and_then(|audit| serialize_payload(&ApiEnvelope::new(audit))),
        "/v1/config" => crate::observability::collect_redacted_config()
            .and_then(|config| serialize_payload(&ApiEnvelope::new(config))),
        "/v1/icm" => serialize_payload(&ApiEnvelope::new(crate::icm_bridge::check(icm_url))),
        _ => return error_response(404, "Not Found", "endpoint not found"),
    };

    match body {
        Ok(body) => HttpResponse {
            status: 200,
            reason: "OK",
            body,
            extra_headers: Vec::new(),
        },
        Err(_) => error_response(500, "Internal Server Error", "endpoint collection failed"),
    }
}

fn serialize_payload<T: Serialize>(payload: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(payload).context("Failed to serialize API payload")
}

fn request_is_authorized(request: &HttpRequest, expected_token: &str) -> bool {
    let Some(value) = request.headers.get("authorization") else {
        return false;
    };
    let parts = value.split_whitespace().collect::<Vec<_>>();
    let [scheme, supplied_token] = parts.as_slice() else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer")
        && constant_time_equal(supplied_token.as_bytes(), expected_token.as_bytes())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let maximum = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

fn error_response(status: u16, reason: &'static str, error: &'static str) -> HttpResponse {
    HttpResponse {
        status,
        reason,
        body: serde_json::to_vec(&ErrorPayload { error })
            .unwrap_or_else(|_| b"{\"error\":\"internal error\"}".to_vec()),
        extra_headers: Vec::new(),
    }
}

fn write_response(stream: &mut TcpStream, response: &HttpResponse) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    )?;
    for (name, value) in &response.extra_headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "\r\n")?;
    stream.write_all(&response.body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::thread;

    fn parsed(path: &str, authorization: Option<&str>) -> HttpRequest {
        let authorization = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        parse_request(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}\r\n").as_bytes(),
        )
        .unwrap()
    }

    fn send(address: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn bind_and_icm_urls_are_loopback_only() {
        assert!(parse_loopback_bind("127.0.0.1:8745").is_ok());
        assert!(parse_loopback_bind("[::1]:8745").is_ok());
        assert!(parse_loopback_bind("0.0.0.0:8745").is_err());
        assert!(parse_loopback_bind("192.0.2.1:8745").is_err());
    }

    #[test]
    fn token_is_required_and_compared_without_prefix_acceptance() {
        assert!(server_token_from(None).is_err());
        assert!(server_token_from(Some("  ".to_string())).is_err());
        let request = parsed("/v1/config", Some("Bearer exact-token"));
        assert!(request_is_authorized(&request, "exact-token"));
        assert!(!request_is_authorized(&request, "exact-token-longer"));
        assert!(!request_is_authorized(
            &parsed("/v1/config", Some("Basic exact-token")),
            "exact-token"
        ));
    }

    #[test]
    fn every_versioned_endpoint_requires_authentication() {
        for path in [
            "/v1/hooks",
            "/v1/agents",
            "/v1/gain",
            "/v1/failures",
            "/v1/audit",
            "/v1/config",
            "/v1/icm",
            "/v1/unknown",
        ] {
            let response = route_request(&parsed(path, None), "secret", None);
            assert_eq!(response.status, 401, "{path}");
        }
        assert_eq!(
            route_request(&parsed("/health", None), "secret", None).status,
            200
        );
    }

    #[test]
    fn socket_protocol_enforces_auth_and_security_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || serve(listener, "server-secret", None, Some(3)));

        let health = send(address, "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(health.starts_with("HTTP/1.1 200 OK"));
        let denied = send(
            address,
            "GET /v1/config HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(denied.starts_with("HTTP/1.1 401 Unauthorized"));
        assert!(denied.contains("WWW-Authenticate: Bearer"));
        let allowed = send(
            address,
            "GET /v1/config HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer server-secret\r\n\r\n",
        );
        assert!(allowed.starts_with("HTTP/1.1 200 OK"));
        assert!(allowed.contains("Cache-Control: no-store"));
        assert!(allowed.contains("X-Content-Type-Options: nosniff"));
        assert!(!allowed.contains("server-secret"));

        server.join().unwrap().unwrap();
    }

    #[test]
    fn parser_rejects_duplicate_authorization_headers() {
        let request = b"GET /v1/config HTTP/1.1\r\nAuthorization: Bearer one\r\nAuthorization: Bearer two\r\n\r\n";
        assert!(parse_request(request).is_err());
    }

    #[test]
    fn request_deadline_stops_slow_header_clients() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request_with_timeout(&mut stream, Duration::from_millis(50))
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(b"GET /health HTTP/1.1\r\n").unwrap();
        let result = server.join().unwrap();
        assert!(result.is_err());
    }
}
