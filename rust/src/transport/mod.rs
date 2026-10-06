//! HTTP transport for the transcription request.
//!
//! Each platform uses what it already has, so Voice Not never ships a TLS stack:
//!
//! * Windows  - WinHTTP, the OS HTTP client.
//! * macOS    - `/usr/bin/curl`, which is part of the base system.
//! * Linux    - `curl` from the distribution.
//!
//! `transport` in the config can force either one.

use std::fmt;

pub mod curl;
#[cfg(windows)]
pub mod winhttp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Platform default: WinHTTP on Windows, curl elsewhere.
    Auto,
    WinHttp,
    Curl,
}

impl Transport {
    pub fn parse(value: &str) -> Transport {
        match value.trim().to_ascii_lowercase().as_str() {
            "winhttp" => Transport::WinHttp,
            "curl" => Transport::Curl,
            _ => Transport::Auto,
        }
    }

    pub fn resolve(self) -> Transport {
        match self {
            Transport::Auto => {
                if cfg!(windows) {
                    Transport::WinHttp
                } else {
                    Transport::Curl
                }
            }
            other => other,
        }
    }
}

/// A parsed endpoint: scheme, host, port and the request target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    /// Path plus query, e.g. `/openai/v1/audio/transcriptions`.
    pub object: String,
    /// The original URL, used verbatim by curl.
    pub url: String,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

/// Splits an `http`/`https` URL into the pieces WinHTTP needs.
pub fn parse_url(url: &str) -> Result<Endpoint, String> {
    let url = url.trim();
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!(
            "unsupported URL '{url}': it must start with http:// or https://"
        ));
    };

    let (authority, object) = match rest.find(['/', '?']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("no host in URL '{url}'"));
    }

    // Bracketed IPv6 literals keep their colons, so look for the closing bracket
    // before splitting off a port.
    let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|_| authority.find(']')) {
        let host = authority[1..end].to_string();
        let remainder = &authority[end + 1..];
        let port = match remainder.strip_prefix(':') {
            Some(text) => text
                .parse::<u16>()
                .map_err(|_| format!("invalid port in '{url}'"))?,
            None => default_port(secure),
        };
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, text)) => (
                host.to_string(),
                text.parse::<u16>()
                    .map_err(|_| format!("invalid port in '{url}'"))?,
            ),
            None => (authority.to_string(), default_port(secure)),
        }
    };

    Ok(Endpoint {
        secure,
        host,
        port,
        object: object.to_string(),
        url: url.to_string(),
    })
}

fn default_port(secure: bool) -> u16 {
    if secure {
        443
    } else {
        80
    }
}

/// POSTs `body` and returns the HTTP status plus the response text.
pub fn post(
    transport: Transport,
    endpoint: &Endpoint,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, String), String> {
    match transport.resolve() {
        Transport::WinHttp => post_winhttp(endpoint, headers, body),
        _ => curl::post(endpoint, headers, body),
    }
}

#[cfg(windows)]
fn post_winhttp(
    endpoint: &Endpoint,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, String), String> {
    winhttp::post(endpoint, headers, body)
}

#[cfg(not(windows))]
fn post_winhttp(
    _endpoint: &Endpoint,
    _headers: &[(String, String)],
    _body: &[u8],
) -> Result<(u16, String), String> {
    Err("the winhttp transport is only available on Windows; use `transport = curl`".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_and_http_urls() {
        let url = parse_url("https://api.groq.com/openai/v1/audio/transcriptions").unwrap();
        assert!(url.secure);
        assert_eq!(url.host, "api.groq.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.object, "/openai/v1/audio/transcriptions");

        let local = parse_url("http://localhost:8080/v1/audio/transcriptions").unwrap();
        assert!(!local.secure, "plain http must not be flagged secure");
        assert_eq!(local.host, "localhost");
        assert_eq!(local.port, 8080);

        let ipv6 = parse_url("http://[::1]:9000/v1/audio/transcriptions").unwrap();
        assert_eq!(ipv6.host, "::1");
        assert_eq!(ipv6.port, 9000);

        let bare = parse_url("https://api.openai.com").unwrap();
        assert_eq!(bare.object, "/");
        assert_eq!(bare.port, 443);
    }

    #[test]
    fn rejects_urls_without_a_scheme() {
        assert!(parse_url("api.groq.com/v1").is_err());
        assert!(parse_url("https://").is_err());
    }

    #[test]
    fn transport_selection() {
        assert_eq!(Transport::parse("curl"), Transport::Curl);
        assert_eq!(Transport::parse("winhttp"), Transport::WinHttp);
        assert_eq!(Transport::parse(""), Transport::Auto);
        // Auto resolves to something usable on every platform.
        assert_ne!(Transport::Auto.resolve(), Transport::Auto);
    }
}
