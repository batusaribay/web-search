use std::io::Read;
use std::time::Duration;

const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

// The HTML endpoints reject unfamiliar agents outright, so requests look like a
// browser. This is the same compromise every terminal search client makes.
pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";

pub struct Fetched {
    /// Final URL after redirects.
    pub url: String,
    pub content_type: String,
    pub body: String,
}

pub fn client(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(timeout)
        // ureq defaults connect attempts to 30s and lets that override the
        // overall timeout, so a blackholed host outlives `-t` without this.
        .timeout_connect(timeout.min(Duration::from_secs(5)))
        .user_agent(USER_AGENT)
        .build()
}

pub fn get(client: &ureq::Agent, url: &str, accept: &str) -> Result<Fetched, String> {
    get_with(client, url, accept, &[])
}

pub fn get_with(
    client: &ureq::Agent,
    url: &str,
    accept: &str,
    headers: &[(&str, &str)],
) -> Result<Fetched, String> {
    let mut request = client
        .get(url)
        .set("Accept", accept)
        .set("Accept-Language", ACCEPT_LANGUAGE);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    handle(request.call())
}

pub fn post_form(
    client: &ureq::Agent,
    url: &str,
    form: &[(&str, &str)],
) -> Result<Fetched, String> {
    let request = client
        .post(url)
        .set("Accept", "text/html,application/xhtml+xml")
        .set("Accept-Language", ACCEPT_LANGUAGE);
    handle(request.send_form(form))
}

fn is_binary(content_type: &str) -> bool {
    let content_type = content_type.to_lowercase();
    content_type.contains("pdf")
        || content_type.contains("octet-stream")
        || content_type.starts_with("image/")
        || content_type.starts_with("audio/")
        || content_type.starts_with("video/")
}

fn handle(result: Result<ureq::Response, ureq::Error>) -> Result<Fetched, String> {
    match result {
        Ok(response) => {
            let url = response.get_url().to_string();
            let content_type = response
                .header("content-type")
                .unwrap_or_default()
                .to_string();

            // Binary payloads are never useful to a text client; skipping the
            // body keeps a PDF or image from failing on UTF-8 decoding.
            if is_binary(&content_type) {
                return Ok(Fetched {
                    url,
                    content_type,
                    body: String::new(),
                });
            }

            let mut body = String::new();
            response
                .into_reader()
                .take(MAX_RESPONSE_BYTES)
                .read_to_string(&mut body)
                .map_err(|error| format!("could not read response: {error}"))?;
            Ok(Fetched {
                url,
                content_type,
                body,
            })
        }
        Err(ureq::Error::Status(code, response)) => {
            let text = response.status_text();
            if text.is_empty() {
                Err(format!("HTTP {code}"))
            } else {
                Err(format!("HTTP {code} {text}"))
            }
        }
        Err(ureq::Error::Transport(error)) => match error.message() {
            Some(message) => Err(format!("request failed: {message}")),
            None => Err(format!("request failed: {}", error.kind())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::is_binary;

    #[test]
    fn detects_binary_payloads() {
        assert!(is_binary("application/pdf"));
        assert!(is_binary("image/png; charset=binary"));
        assert!(!is_binary("text/html; charset=utf-8"));
        assert!(!is_binary(""));
    }
}
