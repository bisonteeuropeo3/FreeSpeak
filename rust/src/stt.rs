//! Groq / OpenAI-compatible speech-to-text client.
//!
//! The request is a plain OpenAI-style `multipart/form-data` POST to
//! `{base_url}/audio/transcriptions`, so any provider that implements that
//! endpoint works: Groq, OpenAI, Mistral, Together, a local server, and so on.
//! The multipart body is assembled by hand so the `file` part can be forced
//! last, which is the ordering those endpoints expect.

use crate::config::Config;
use crate::transport::{self, Endpoint, Transport};

const BOUNDARY: &str = "----voicenot7f3a9c2b1e4d";

/// Most providers cap uploads at 25 MB; Groq's free tier does.
const MAX_UPLOAD_BYTES: usize = 24 * 1024 * 1024;

fn text_field(body: &mut Vec<u8>, name: &str, value: &str) {
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )
        .as_bytes(),
    );
}

fn build_body(cfg: &Config, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 1024);
    text_field(&mut body, "model", cfg.model.trim());
    if !cfg.language.trim().is_empty() {
        text_field(&mut body, "language", cfg.language.trim());
    }
    if !cfg.prompt.trim().is_empty() {
        // Whisper's prompt slot: vocabulary biasing and style steering.
        text_field(&mut body, "prompt", cfg.prompt.trim());
    }
    text_field(&mut body, "response_format", "json");
    text_field(&mut body, "temperature", "0");

    // The file part goes last.
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

fn clip(text: &str, limit: usize) -> String {
    let cleaned = text.replace(['\r', '\n'], " ");
    if cleaned.chars().count() <= limit {
        cleaned
    } else {
        cleaned.chars().take(limit).collect::<String>() + "..."
    }
}

/// Sends the WAV to the API and returns the transcript.
pub fn transcribe(cfg: &Config, wav: &[u8]) -> Result<String, String> {
    if wav.len() > MAX_UPLOAD_BYTES {
        return Err(format!(
            "the recording is {:.1} MB, over the usual 25 MB upload limit - dictate in shorter chunks",
            wav.len() as f64 / (1024.0 * 1024.0)
        ));
    }

    let url = cfg.endpoint();
    let endpoint = transport::parse_url(&url)?;
    if !has_api_key(cfg) {
        return Err(format!(
            "no API key configured - run `voice-not --set-key`, or add `api_key = ...` to {}",
            crate::config::config_path().display()
        ));
    }

    let body = build_body(cfg, wav);
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {}", cfg.api_key)),
        (
            "Content-Type".to_string(),
            format!("multipart/form-data; boundary={BOUNDARY}"),
        ),
    ];

    let transport_choice = cfg.transport();
    let (status, text) = transport::post(transport_choice, &endpoint, &headers, &body)?;
    interpret(status, &text, &endpoint, transport_choice)
}

fn has_api_key(cfg: &Config) -> bool {
    !cfg.api_key.trim().is_empty()
}

fn interpret(
    status: u16,
    text: &str,
    endpoint: &Endpoint,
    transport_choice: Transport,
) -> Result<String, String> {
    if !(200..300).contains(&status) {
        let lowered = text.to_ascii_lowercase();
        if lowered.contains("api key") || lowered.contains("unauthorized") || status == 401 || status == 403 {
            return Err(format!(
                "the provider rejected the API key - check it with `voice-not --set-key`: {}",
                clip(text, 200)
            ));
        }
        if status == 404 {
            return Err(format!(
                "{} has no /audio/transcriptions endpoint - check `base_url` in the config: {}",
                endpoint.host,
                clip(text, 200)
            ));
        }
        return Err(match status {
            429 => format!("rate limited by {} (HTTP 429): {}", endpoint.host, clip(text, 200)),
            0 => format!(
                "no HTTP response from {} (check the URL, and the network)",
                endpoint.host
            ),
            _ => format!(
                "{} returned HTTP {status} via {:?}: {}",
                endpoint.host,
                transport_choice.resolve(),
                clip(text, 300)
            ),
        });
    }
    parse_transcript(text)
}

fn parse_transcript(raw: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("unexpected API response: {e}"))?;
    match value.get("text").and_then(|t| t.as_str()) {
        Some(text) => Ok(text.to_string()),
        None => Err(format!("API response had no 'text' field: {}", clip(raw, 300))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_part_is_last_and_body_is_terminated() {
        let cfg = Config {
            api_key: "k".into(),
            language: "en".into(),
            prompt: "Kubernetes, pgvector".into(),
            ..Config::default()
        };
        let body = build_body(&cfg, b"WAVDATA");
        let text = String::from_utf8_lossy(&body).to_string();

        assert!(text.contains("name=\"model\""));
        assert!(text.contains("name=\"language\""));
        assert!(text.contains("name=\"prompt\""));
        assert!(text.contains("name=\"response_format\""));
        // The file part must come after every other field.
        let file_at = text.find("name=\"file\"").expect("file part");
        let prompt_at = text.rfind("name=\"prompt\"").expect("prompt part");
        assert!(prompt_at < file_at);
        assert!(text.ends_with(&format!("--{BOUNDARY}--\r\n")));
        assert!(text.contains("WAVDATA"));
    }

    #[test]
    fn parses_text_out_of_the_response() {
        let parsed = parse_transcript(r#"{"text":"hello there"}"#).unwrap();
        assert_eq!(parsed, "hello there");
        assert!(parse_transcript("{}").is_err());
    }

    #[test]
    fn omits_optional_fields_when_blank() {
        let cfg = Config {
            language: String::new(),
            prompt: String::new(),
            ..Config::default()
        };
        let text = String::from_utf8_lossy(&build_body(&cfg, b"x")).to_string();
        assert!(!text.contains("name=\"language\""));
        assert!(!text.contains("name=\"prompt\""));
    }

    #[test]
    fn refuses_uploads_over_the_limit() {
        let cfg = Config::default();
        let huge = vec![0u8; MAX_UPLOAD_BYTES + 1];
        let err = transcribe(&cfg, &huge).unwrap_err();
        assert!(err.contains("25 MB"), "unexpected error: {err}");
    }

    #[test]
    fn reports_a_bad_key_helpfully() {
        let endpoint = transport::parse_url("https://api.groq.com/openai/v1/audio/transcriptions").unwrap();
        let err = interpret(
            400,
            r#"{"error":"Incorrect API key provided"}"#,
            &endpoint,
            Transport::Auto,
        )
        .unwrap_err();
        assert!(err.contains("rejected the API key"), "unexpected: {err}");
        assert!(err.contains("--set-key"));
    }

    #[test]
    fn reports_a_wrong_base_url_helpfully() {
        let endpoint = transport::parse_url("https://example.com/v1/audio/transcriptions").unwrap();
        let err = interpret(404, "not found", &endpoint, Transport::Auto).unwrap_err();
        assert!(err.contains("/audio/transcriptions"), "unexpected: {err}");
        assert!(err.contains("base_url"));
    }
}
