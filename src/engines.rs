use std::collections::HashSet;
use std::env;
use std::io::Read;
use std::sync::LazyLock;
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

// The HTML endpoints reject unfamiliar agents outright, so requests look like a
// browser. This is the same compromise every terminal search client makes.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0";

const SEARX_INSTANCES: [&str; 4] = [
    "https://searx.be",
    "https://search.inetol.net",
    "https://baresearch.org",
    "https://priv.au",
];

static RESULT: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div.result").expect("valid result selector"));
static RESULT_LINK: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("a.result__a").expect("valid result link selector"));
static RESULT_SNIPPET: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".result__snippet").expect("valid snippet selector"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub engine: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Auto,
    Bing,
    DuckDuckGo,
    Searx,
    Brave,
}

impl Engine {
    pub const NAMES: &str = "auto, bing, ddg, searx, brave";

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "bing" => Some(Self::Bing),
            "ddg" | "duckduckgo" | "duck-duck-go" => Some(Self::DuckDuckGo),
            "searx" | "searxng" => Some(Self::Searx),
            "brave" => Some(Self::Brave),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Bing => "bing",
            Self::DuckDuckGo => "ddg",
            Self::Searx => "searx",
            Self::Brave => "brave",
        }
    }
}

pub struct Options {
    pub limit: usize,
    pub timeout: Duration,
    pub searx_url: Option<String>,
}

pub fn search(engine: Engine, query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    match engine {
        Engine::Auto => search_auto(query, options),
        Engine::Bing => bing(query, options),
        Engine::DuckDuckGo => duckduckgo(query, options),
        Engine::Searx => searx(query, options),
        Engine::Brave => brave(query, options),
    }
}

fn search_auto(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let mut failures = Vec::new();
    let mut empty = None;

    for engine in [Engine::DuckDuckGo, Engine::Bing, Engine::Searx] {
        match search(engine, query, options) {
            Ok(hits) if !hits.is_empty() => return Ok(hits),
            Ok(hits) => empty = Some(hits),
            Err(message) => failures.push(format!("{}: {message}", engine.name())),
        }
    }

    match empty {
        Some(hits) => Ok(hits),
        None => Err(failures.join("; ")),
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .build()
}

fn fetch(request: ureq::Request) -> Result<String, String> {
    let result = request.set("Accept-Language", ACCEPT_LANGUAGE).call();
    handle(result)
}

fn fetch_form(url: &str, timeout: Duration, form: &[(&str, &str)]) -> Result<String, String> {
    let request = agent(timeout)
        .post(url)
        .set("Accept", "text/html,application/xhtml+xml")
        .set("Accept-Language", ACCEPT_LANGUAGE);
    let result = request.send_form(form);
    handle(result)
}

fn handle(result: Result<ureq::Response, ureq::Error>) -> Result<String, String> {
    match result {
        Ok(response) => read_body(response),
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

fn read_body(response: ureq::Response) -> Result<String, String> {
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES)
        .read_to_string(&mut body)
        .map_err(|error| format!("could not read response: {error}"))?;
    Ok(body)
}

fn encode(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

fn bing(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let agent = agent(options.timeout);
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = HashSet::new();
    let mut first = 1;

    while hits.len() < options.limit && first <= 41 {
        let url = format!(
            "https://www.bing.com/search?q={}&format=rss&count=10&first={first}",
            encode(query)
        );
        let request = agent.get(&url).set(
            "Accept",
            "application/rss+xml, application/xml;q=0.9, */*;q=0.8",
        );
        let body = fetch(request)?;
        let page = parse_bing_rss(&body)?;
        let before = hits.len();

        for hit in page {
            if seen.insert(hit.url.clone()) {
                hits.push(hit);
            }
        }

        // Bing sometimes ignores the page offset; stop instead of looping.
        if hits.len() == before {
            break;
        }
        first += 10;
    }

    hits.truncate(options.limit);
    Ok(hits)
}

#[derive(Clone, Copy)]
enum RssField {
    Title,
    Link,
    Description,
}

impl RssField {
    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "title" => Some(Self::Title),
            "link" => Some(Self::Link),
            "description" => Some(Self::Description),
            _ => None,
        }
    }

    fn append(self, hit: &mut Hit, text: &str) {
        match self {
            Self::Title => hit.title.push_str(text),
            Self::Link => hit.url.push_str(text),
            Self::Description => hit.snippet.push_str(text),
        }
    }
}

fn parse_bing_rss(xml: &str) -> Result<Vec<Hit>, String> {
    let mut reader = Reader::from_str(xml);

    let mut hits = Vec::new();
    let mut item: Option<Hit> = None;
    let mut field: Option<RssField> = None;

    loop {
        let event = reader
            .read_event()
            .map_err(|error| format!("bing returned malformed XML: {error}"))?;

        match event {
            Event::Start(start) => {
                if start.name().as_ref() == "item" {
                    item = Some(Hit {
                        title: String::new(),
                        url: String::new(),
                        snippet: String::new(),
                        engine: "bing",
                    });
                    field = None;
                } else if item.is_some() {
                    field = RssField::from_tag(start.name().as_ref());
                }
            }
            Event::End(end) => {
                if end.name().as_ref() == "item"
                    && let Some(mut hit) = item.take()
                {
                    finish(&mut hit);
                    if !hit.url.is_empty() {
                        hits.push(hit);
                    }
                }
                field = None;
            }
            Event::Text(text) => {
                if let (Some(hit), Some(kind)) = (item.as_mut(), field) {
                    kind.append(hit, &text.into_inner());
                }
            }
            Event::CData(data) => {
                if let (Some(hit), Some(kind)) = (item.as_mut(), field) {
                    kind.append(hit, &data.into_inner());
                }
            }
            Event::GeneralRef(reference) => {
                if let (Some(hit), Some(kind)) = (item.as_mut(), field) {
                    let text = resolve_reference(&reference);
                    kind.append(hit, &text);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(hits)
}

fn resolve_reference(reference: &BytesRef) -> String {
    if let Ok(Some(character)) = reference.resolve_char_ref() {
        return character.to_string();
    }

    match reference.clone().into_inner().as_ref() {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" | "#39" => "'".to_string(),
        "nbsp" => " ".to_string(),
        _ => String::new(),
    }
}

fn duckduckgo(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let body = fetch_form(
        "https://html.duckduckgo.com/html/",
        options.timeout,
        &[("q", query), ("kl", "us-en")],
    )?;

    let hits = parse_duckduckgo_html(&body);
    if hits.is_empty() && is_bot_check(&body) {
        return Err("blocked by DuckDuckGo's bot check; try --engine bing".to_string());
    }

    let mut seen = HashSet::new();
    Ok(hits
        .into_iter()
        .filter(|hit| seen.insert(hit.url.clone()))
        .take(options.limit)
        .collect())
}

fn is_bot_check(html: &str) -> bool {
    html.contains("anomaly-modal")
        || html.contains("bots use DuckDuckGo")
        || html.contains("Unfortunately, bots")
}

fn parse_duckduckgo_html(html: &str) -> Vec<Hit> {
    let document = Html::parse_document(html);
    let mut hits = Vec::new();

    for result in document.select(&RESULT) {
        if result.value().classes().any(|class| class == "result--ad") {
            continue;
        }

        let Some(link) = result.select(&RESULT_LINK).next() else {
            continue;
        };
        let Some(href) = link.value().attr("href") else {
            continue;
        };

        let title = clean_text(&link.text().collect::<String>());
        let url = absolute_url(href);
        if title.is_empty() || url.is_empty() {
            continue;
        }

        let snippet = result
            .select(&RESULT_SNIPPET)
            .next()
            .map(|element| clean_text(&element.text().collect::<String>()))
            .unwrap_or_default();

        hits.push(Hit {
            title,
            url,
            snippet,
            engine: "ddg",
        });
    }

    hits
}

fn searx(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let configured = options
        .searx_url
        .clone()
        .or_else(|| env::var("SEARX_URL").ok())
        .filter(|url| !url.trim().is_empty());

    let instances: Vec<String> = match configured {
        Some(url) => vec![url],
        None => SEARX_INSTANCES.iter().map(|url| url.to_string()).collect(),
    };

    let mut failures = Vec::new();

    for base in instances {
        match searx_instance(&base, query, options) {
            Ok(hits) if !hits.is_empty() => return Ok(hits),
            Ok(_) => failures.push(format!("{base}: no results")),
            Err(message) => failures.push(format!("{base}: {message}")),
        }
    }

    Err(failures.join("; "))
}

fn searx_instance(base: &str, query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let base = base.trim_end_matches('/');
    let agent = agent(options.timeout);
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = HashSet::new();
    let mut page = 1;

    while hits.len() < options.limit && page <= 3 {
        let url = format!(
            "{base}/search?q={}&format=json&language=en&safesearch=0&pageno={page}",
            encode(query)
        );
        let body = fetch(agent.get(&url).set("Accept", "application/json"))?;
        let results = parse_searx_json(&body)?;

        let before = hits.len();
        for hit in results {
            if seen.insert(hit.url.clone()) {
                hits.push(hit);
            }
        }

        if hits.len() == before {
            break;
        }
        page += 1;
    }

    hits.truncate(options.limit);
    Ok(hits)
}

fn parse_searx_json(body: &str) -> Result<Vec<Hit>, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| "instance did not return JSON (JSON output may be disabled)".to_string())?;

    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "unexpected response shape".to_string())?;

    let mut hits = Vec::new();
    for result in results {
        let Some(url) = result.get("url").and_then(Value::as_str) else {
            continue;
        };
        let title = result
            .get("title")
            .and_then(Value::as_str)
            .map(clean_text)
            .unwrap_or_default();
        if title.is_empty() || url.is_empty() {
            continue;
        }

        let snippet = result
            .get("content")
            .and_then(Value::as_str)
            .map(clean_text)
            .unwrap_or_default();

        hits.push(Hit {
            title,
            url: url.to_string(),
            snippet,
            engine: "searx",
        });
    }

    Ok(hits)
}

fn brave(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let key = env::var("BRAVE_API_KEY")
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            "BRAVE_API_KEY is not set; create a key at https://brave.com/search/api/".to_string()
        })?;

    let agent = agent(options.timeout);
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0;

    while hits.len() < options.limit && offset < 10 {
        let count = (options.limit - hits.len()).min(20);
        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={count}&offset={offset}",
            encode(query)
        );
        let body = fetch(
            agent
                .get(&url)
                .set("Accept", "application/json")
                .set("X-Subscription-Token", &key),
        )?;
        let results = parse_brave_json(&body)?;

        let before = hits.len();
        for hit in results {
            if seen.insert(hit.url.clone()) {
                hits.push(hit);
            }
        }

        if hits.len() == before {
            break;
        }
        offset += 1;
    }

    hits.truncate(options.limit);
    Ok(hits)
}

fn parse_brave_json(body: &str) -> Result<Vec<Hit>, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| "Brave returned a response that was not JSON".to_string())?;

    let results = match value.pointer("/web/results").and_then(Value::as_array) {
        Some(results) => results,
        None => return Ok(Vec::new()),
    };

    let mut hits = Vec::new();
    for result in results {
        let Some(url) = result.get("url").and_then(Value::as_str) else {
            continue;
        };
        let title = result
            .get("title")
            .and_then(Value::as_str)
            .map(clean_text)
            .unwrap_or_default();

        if title.is_empty() || url.is_empty() {
            continue;
        }

        let snippet = result
            .get("description")
            .and_then(Value::as_str)
            .map(clean_text)
            .unwrap_or_default();

        hits.push(Hit {
            title,
            url: url.to_string(),
            snippet,
            engine: "brave",
        });
    }

    Ok(hits)
}

fn absolute_url(href: &str) -> String {
    let href = href.trim();
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_string()
    };

    let Ok(url) = Url::parse(&absolute) else {
        return absolute;
    };

    if url.host_str() == Some("duckduckgo.com") {
        for (name, value) in url.query_pairs() {
            if name == "uddg" {
                return value.into_owned();
            }
        }
    }

    absolute
}

fn finish(hit: &mut Hit) {
    hit.title = clean_text(&hit.title);
    hit.url = hit.url.trim().to_string();
    hit.snippet = clean_text(&hit.snippet);
}

/// Turns result markup into one line of readable text: tags removed, entities
/// decoded, whitespace collapsed.
fn clean_text(input: &str) -> String {
    let text = decode_entities(&strip_tags(input));

    let mut cleaned = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !cleaned.is_empty() {
            cleaned.push(' ');
        }
        cleaned.push_str(word);
    }
    cleaned
}

/// Removes HTML tags. A `<` that does not start a tag is kept, so text like
/// "a < b" survives.
fn strip_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut index = 0;

    while let Some(offset) = input[index..].find('<') {
        let start = index + offset;
        output.push_str(&input[index..start]);

        let rest = &input[start + 1..];
        let tag_like = rest.starts_with(|character: char| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '!' | '?')
        });

        match (tag_like, rest.find('>')) {
            (true, Some(end)) => index = start + 1 + end + 1,
            _ => {
                output.push('<');
                index = start + 1;
            }
        }
    }

    output.push_str(&input[index..]);
    output
}

fn decode_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        let after = &rest[start + 1..];

        let Some(end) = after.find(';') else {
            output.push('&');
            rest = after;
            continue;
        };

        let entity = &after[..end];
        match entity {
            "amp" => output.push('&'),
            "lt" => output.push('<'),
            "gt" => output.push('>'),
            "quot" => output.push('"'),
            "apos" | "#39" => output.push('\''),
            "nbsp" => output.push(' '),
            _ => match decode_numeric_entity(entity) {
                Some(character) => output.push(character),
                None => {
                    output.push('&');
                    output.push_str(entity);
                    output.push(';');
                }
            },
        }
        rest = &after[end + 1..];
    }

    output.push_str(rest);
    output
}

fn decode_numeric_entity(entity: &str) -> Option<char> {
    let number = entity.strip_prefix('#')?;
    let code = match number.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bing_rss_items() {
        let xml = r#"<?xml version="1.0"?><rss><channel>
            <item>
                <title>Rust &amp; Cargo</title>
                <link>https://www.rust-lang.org/</link>
                <description>Fast &amp; memory safe. &#8212; docs</description>
            </item>
            <item>
                <title><![CDATA[A < b]]></title>
                <link>https://example.com/</link>
                <description>Plain text</description>
            </item>
        </channel></rss>"#;

        let hits = parse_bing_rss(xml).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust & Cargo");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].snippet, "Fast & memory safe. \u{2014} docs");
        assert_eq!(hits[0].engine, "bing");
        assert_eq!(hits[1].title, "A < b");
    }

    #[test]
    fn skips_bing_items_without_a_link() {
        let xml = r#"<rss><channel><item><title>Broken</title></item></channel></rss>"#;
        assert!(parse_bing_rss(xml).unwrap().is_empty());
    }

    #[test]
    fn parses_duckduckgo_results_and_skips_ads() {
        let html = r#"<html><body>
            <div class="result results_links result--ad">
                <h2><a class="result__a" href="https://ad.example/">Buy things</a></h2>
            </div>
            <div class="result results_links">
                <h2 class="result__title">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=abc">
                        Rust <b>Programming</b> Language
                    </a>
                </h2>
                <a class="result__snippet" href="https://www.rust-lang.org/">
                    A language <b>empowering</b> everyone.
                </a>
                <span class="result__url">rust-lang.org</span>
            </div>
        </body></html>"#;

        let hits = parse_duckduckgo_html(html);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].snippet, "A language empowering everyone.");
    }

    #[test]
    fn detects_duckduckgo_bot_checks() {
        assert!(is_bot_check(r#"<div id="anomaly-modal"></div>"#));
        assert!(!is_bot_check("<div class=\"result\"></div>"));
    }

    #[test]
    fn parses_searx_json() {
        let json = r#"{"results": [
            {"title": "Rust", "url": "https://www.rust-lang.org/", "content": "A language."},
            {"title": "No URL"}
        ]}"#;

        let hits = parse_searx_json(json).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust");
        assert_eq!(hits[0].engine, "searx");
    }

    #[test]
    fn rejects_non_json_searx_responses() {
        assert!(parse_searx_json("<html>nope</html>").is_err());
    }

    #[test]
    fn parses_brave_json_and_allows_missing_results() {
        let json = r#"{"web": {"results": [
            {"title": "Rust <strong>lang</strong>", "url": "https://www.rust-lang.org/", "description": "Fast."}
        ]}}"#;

        let hits = parse_brave_json(json).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust lang");
        assert_eq!(hits[0].engine, "brave");
        assert!(parse_brave_json("{}").unwrap().is_empty());
    }

    #[test]
    fn resolves_duckduckgo_redirects() {
        assert_eq!(
            absolute_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1&rut=x"),
            "https://example.com/a?b=1"
        );
        assert_eq!(absolute_url("https://example.com/"), "https://example.com/");
        assert_eq!(absolute_url("/relative"), "/relative");
    }

    #[test]
    fn cleans_markup_and_entities() {
        assert_eq!(
            clean_text("  A  <b>bold</b>\n move &amp; &#8212; &#x41; &nbsp;ok  "),
            "A bold move & \u{2014} A ok"
        );
        assert_eq!(
            clean_text("unknown &weird; entity"),
            "unknown &weird; entity"
        );
        assert_eq!(clean_text("dangling & ampersand"), "dangling & ampersand");
    }

    #[test]
    fn parses_engine_names() {
        assert_eq!(Engine::parse("DDG"), Some(Engine::DuckDuckGo));
        assert_eq!(Engine::parse("searxng"), Some(Engine::Searx));
        assert_eq!(Engine::parse("nope"), None);
    }
}
