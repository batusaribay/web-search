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
    referer: &str,
    form: &[(&str, &str)],
) -> Result<Fetched, String> {
    // DuckDuckGo's no-JS form is meant to be submitted by a browser, so send
    // the headers a browser sends. Missing Sec-Fetch/Referer headers make the
    // request look like a scraper before the IP reputation even matters.
    let origin = referer.trim_end_matches('/');
    let request = client
        .post(url)
        .set(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .set("Accept-Language", ACCEPT_LANGUAGE)
        .set("Cache-Control", "max-age=0")
        .set("Origin", origin)
        .set("Referer", referer)
        .set("Sec-Fetch-Dest", "document")
        .set("Sec-Fetch-Mode", "navigate")
        .set("Sec-Fetch-Site", "same-origin")
        .set("Sec-Fetch-User", "?1")
        .set("Upgrade-Insecure-Requests", "1");
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

            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_RESPONSE_BYTES)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("could not read response: {error}"))?;
            Ok(Fetched {
                url,
                content_type,
                body: String::from_utf8_lossy(&bytes).into_owned(),
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    use super::*;

    #[test]
    fn detects_binary_payloads() {
        assert!(is_binary("application/pdf"));
        assert!(is_binary("image/png; charset=binary"));
        assert!(!is_binary("text/html; charset=utf-8"));
        assert!(!is_binary(""));
    }

    #[test]
    fn sends_a_browser_like_form() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");

            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            let header_end = loop {
                let read = stream.read(&mut buffer).expect("read headers");
                assert!(read > 0, "client closed before sending headers");
                request.extend_from_slice(&buffer[..read]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };

            let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);

            let mut body = request[header_end..].to_vec();
            while body.len() < length {
                let read = stream.read(&mut buffer).expect("read body");
                if read == 0 {
                    break;
                }
                body.extend_from_slice(&buffer[..read]);
            }

            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("write response");

            format!("{headers}\r\n{}", String::from_utf8_lossy(&body))
        });

        let referer = format!("http://{address}/");
        let client = client(Duration::from_secs(5));
        let fetched = post_form(
            &client,
            &format!("http://{address}/search"),
            &referer,
            &[("q", "dashed filename"), ("b", ""), ("kl", "us-en")],
        )
        .expect("post form");
        assert_eq!(fetched.body, "ok");

        let request = server.join().expect("server").to_ascii_lowercase();
        assert!(request.contains("sec-fetch-dest: document"));
        assert!(request.contains("sec-fetch-mode: navigate"));
        assert!(request.contains("sec-fetch-site: same-origin"));
        assert!(request.contains("sec-fetch-user: ?1"));
        assert!(request.contains(&format!("referer: {referer}").to_ascii_lowercase()));
        assert!(request.contains(&format!(
            "origin: {}",
            referer.trim_end_matches('/').to_ascii_lowercase()
        )));
        assert!(request.contains("q=dashed+filename"));
        assert!(request.contains("b="));
        assert!(request.contains("kl=us-en"));
    }
}
