//! Optional ICM integration over its loopback HTTP API.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

const MAX_HEALTH_BODY: u64 = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IcmHealth {
    pub configured: bool,
    pub reachable: bool,
    pub status: String,
    pub detail: String,
}

impl IcmHealth {
    pub fn not_configured() -> Self {
        Self {
            configured: false,
            reachable: false,
            status: "not-configured".to_string(),
            detail: "pass --icm-url to enable the localhost ICM bridge".to_string(),
        }
    }
}

pub fn check(icm_url: Option<&str>) -> IcmHealth {
    let Some(icm_url) = icm_url else {
        return IcmHealth::not_configured();
    };
    let base = match normalize_loopback_url(icm_url) {
        Ok(base) => base,
        Err(error) => {
            return IcmHealth {
                configured: true,
                reachable: false,
                status: "invalid-url".to_string(),
                detail: error.to_string(),
            };
        }
    };
    let health_url = format!("{base}/health");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(750))
        .timeout_read(Duration::from_millis(1250))
        .timeout_write(Duration::from_millis(750))
        .build();

    match agent.get(&health_url).call() {
        Ok(response) => {
            let status_code = response.status();
            let mut body = String::new();
            let read_result = response
                .into_reader()
                .take(MAX_HEALTH_BODY)
                .read_to_string(&mut body);
            if let Err(error) = read_result {
                return IcmHealth {
                    configured: true,
                    reachable: true,
                    status: "invalid-response".to_string(),
                    detail: format!("ICM health response could not be read: {error}"),
                };
            }
            let reported_status = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "ok".to_string());
            IcmHealth {
                configured: true,
                reachable: true,
                status: reported_status,
                detail: format!("ICM health endpoint returned HTTP {status_code}"),
            }
        }
        Err(ureq::Error::Status(code, _)) => IcmHealth {
            configured: true,
            reachable: true,
            status: "http-error".to_string(),
            detail: format!("ICM health endpoint returned HTTP {code}"),
        },
        Err(error) => IcmHealth {
            configured: true,
            reachable: false,
            status: "unreachable".to_string(),
            detail: format!("ICM health request failed: {error}"),
        },
    }
}

pub fn normalize_loopback_url(value: &str) -> Result<String> {
    normalize_loopback_http_url(value, "ICM")
}

pub fn normalize_loopback_http_url(value: &str, subject: &str) -> Result<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.contains('?') || trimmed.contains('#') {
        anyhow::bail!("{subject} URL must not contain a query or fragment");
    }
    let authority_and_path = trimmed
        .strip_prefix("http://")
        .with_context(|| format!("{subject} URL must use http:// on localhost"))?;
    let authority = authority_and_path.split('/').next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains('?')
        || authority.contains('#')
    {
        anyhow::bail!("{subject} URL has an invalid authority");
    }

    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed
            .split_once(']')
            .map(|(host, _)| host)
            .with_context(|| format!("{subject} URL has an invalid IPv6 host"))?
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if port.chars().all(|character| character.is_ascii_digit()) {
            host
        } else {
            authority
        }
    } else {
        authority
    };

    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !is_loopback {
        anyhow::bail!("{subject} URL must resolve explicitly to localhost or a loopback IP");
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn rejects_non_loopback_and_credentialed_urls() {
        for value in [
            "https://127.0.0.1:8746",
            "http://192.0.2.10:8746",
            "http://token@127.0.0.1:8746",
            "http://127.0.0.1:8746?token=value",
        ] {
            assert!(normalize_loopback_url(value).is_err(), "{value}");
        }
        assert_eq!(
            normalize_loopback_url("http://[::1]:8746/").unwrap(),
            "http://[::1]:8746"
        );
        assert_eq!(
            normalize_loopback_url("http://localhost:8746").unwrap(),
            "http://localhost:8746"
        );
    }

    #[test]
    fn reads_health_from_local_http_api() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("GET /health "));
            let body = r#"{"status":"healthy"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let health = check(Some(&format!("http://{address}")));
        server.join().unwrap();
        assert!(health.configured);
        assert!(health.reachable);
        assert_eq!(health.status, "healthy");
    }
}
