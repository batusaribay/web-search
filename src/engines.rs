use std::collections::HashSet;
use std::env;
use std::sync::LazyLock;
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use crate::browser;
use crate::cache;
use crate::http;

const SEARX_INSTANCES: [&str; 1] = ["https://search.lumy.live"];

static RESULT: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div.result").expect("valid result selector"));
static RESULT_LINK: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("a.result__a").expect("valid result link selector"));
static RESULT_SNIPPET: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".result__snippet").expect("valid snippet selector"));
static LITE_LINK: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("a.result-link").expect("valid lite result link selector"));
static LITE_SNIPPET: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("td.result-snippet").expect("valid lite snippet selector"));
static ROW: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("tr").expect("valid row selector"));

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Http,
    Obscura,
}

impl Backend {
    pub const NAMES: &str = "http, obscura";

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "http" => Some(Self::Http),
            "obscura" | "browser" => Some(Self::Obscura),
            _ => None,
        }
    }
}

pub struct Options {
    pub limit: usize,
    pub timeout: Duration,
    pub searx_url: Option<String>,
    pub verbose: bool,
    pub backend: Backend,
    pub obscura: String,
    pub stealth: bool,
}

pub fn search(engine: Engine, query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    if options.backend == Backend::Obscura {
        return browser::search(engine, query, options);
    }

    match engine {
        Engine::Auto => search_auto(query, options),
        Engine::Bing => bing(query, options),
        Engine::DuckDuckGo => duckduckgo(query, options),
        Engine::Searx => searx(query, options),
        Engine::Brave => brave(query, options),
    }
}

fn brave_configured() -> bool {
    env::var("BRAVE_API_KEY").is_ok_and(|key| !key.trim().is_empty())
}

fn search_auto(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let mut failures = Vec::new();
    let mut empty = None;
    let mut empty_engine = None;

    let mut engines = vec![Engine::DuckDuckGo];
    if brave_configured() {
        engines.push(Engine::Brave);
    }
    engines.push(Engine::Searx);
    engines.push(Engine::Bing);

    for engine in engines {
        if options.verbose {
            eprintln!("web-search: trying {}", engine.name());
        }
        match search(engine, query, options) {
            Ok(hits) if !hits.is_empty() => {
                if options.verbose && engine != Engine::DuckDuckGo {
                    eprintln!("web-search: {} returned results", engine.name());
                }
                return Ok(hits);
            }
            Ok(_) => {
                if options.verbose {
                    eprintln!("web-search: {}: no results", engine.name());
                }
                empty = Some(Vec::new());
                empty_engine = Some(engine.name());
            }
            Err(message) => {
                if options.verbose {
                    eprintln!("web-search: {}: {message}", engine.name());
                }
                failures.push(format!("{}: {message}", engine.name()));
            }
        }
    }

    match empty {
        Some(hits) if failures.is_empty() => Ok(hits),
        Some(_) => {
            if let Some(engine) = empty_engine {
                failures.push(format!("{engine}: no results"));
            }
            Err(failures.join("; "))
        }
        None => Err(failures.join("; ")),
    }
}

pub(crate) fn encode(query: &str) -> String {
    url::form_urlencoded::byte_serialize(query.as_bytes()).collect()
}

fn bing_url(query: &str, first: usize) -> String {
    format!(
        "https://www.bing.com/search?q={}&format=rss&count=10&first={first}&mkt=en-US&setlang=en&cc=us",
        encode(query)
    )
}

fn bing(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let client = http::client(options.timeout);
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = HashSet::new();
    let mut first = 1;

    while hits.len() < options.limit && first <= 41 {
        let url = bing_url(query, first);
        let body = http::get(
            &client,
            &url,
            "application/rss+xml, application/xml;q=0.9, */*;q=0.8",
        )?
        .body;
        let page = keep_relevant(query, parse_bing_rss(&body)?);
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

#[derive(Debug)]
enum DdgError {
    BotCheck,
    Transport(String),
}

impl DdgError {
    fn message(&self) -> String {
        match self {
            Self::BotCheck => {
                "DuckDuckGo is bot-checking this IP (temporary); wait a bit or use --engine bing"
                    .to_string()
            }
            Self::Transport(message) => message.clone(),
        }
    }
}

fn duckduckgo(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    if let Some(remaining) = cache::ddg_cooldown_remaining() {
        let minutes = remaining.as_secs().div_ceil(60);
        return Err(format!(
            "DuckDuckGo is cooling down after a block ({minutes} min left); try --engine bing"
        ));
    }

    if let Some(wait) = cache::ddg_request_wait() {
        std::thread::sleep(wait);
    }
    cache::mark_ddg_request();

    match duckduckgo_html(query, options) {
        Ok(hits) if !hits.is_empty() => Ok(hits),
        // A clean empty page is a real answer. Do not make a second request.
        Ok(_) => Ok(Vec::new()),
        Err(DdgError::BotCheck) => {
            cache::mark_ddg_blocked();
            Err(DdgError::BotCheck.message())
        }
        Err(DdgError::Transport(html_error)) => match duckduckgo_lite(query, options) {
            Ok(hits) => Ok(hits),
            Err(DdgError::BotCheck) => {
                cache::mark_ddg_blocked();
                Err(DdgError::BotCheck.message())
            }
            Err(DdgError::Transport(lite_error)) => {
                // A network failure is not a bot check. Recording a cooldown
                // here would disable DuckDuckGo for an hour after a transient
                // outage, so only a real block (above) is remembered.
                Err(format!("html: {html_error}; lite: {lite_error}"))
            }
        },
    }
}

fn duckduckgo_html(query: &str, options: &Options) -> Result<Vec<Hit>, DdgError> {
    let client = http::client(options.timeout);
    let body = http::post_form(
        &client,
        "https://html.duckduckgo.com/html/",
        "https://html.duckduckgo.com/",
        &[("q", query), ("b", ""), ("kl", "us-en")],
    )
    .map_err(DdgError::Transport)?
    .body;

    let hits = parse_duckduckgo_html(&body);
    if hits.is_empty() && is_bot_check(&body) {
        return Err(DdgError::BotCheck);
    }

    Ok(dedupe(hits, options.limit))
}

fn duckduckgo_lite(query: &str, options: &Options) -> Result<Vec<Hit>, DdgError> {
    let client = http::client(options.timeout);
    let body = http::post_form(
        &client,
        "https://lite.duckduckgo.com/lite/",
        "https://lite.duckduckgo.com/",
        &[("q", query), ("kl", "us-en")],
    )
    .map_err(DdgError::Transport)?
    .body;

    let hits = parse_duckduckgo_lite(&body);
    if hits.is_empty() && is_bot_check(&body) {
        return Err(DdgError::BotCheck);
    }

    Ok(dedupe(hits, options.limit))
}

fn dedupe(hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut seen = HashSet::new();
    hits.into_iter()
        .filter(|hit| seen.insert(hit.url.clone()))
        .take(limit)
        .collect()
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

fn parse_duckduckgo_lite(html: &str) -> Vec<Hit> {
    let document = Html::parse_document(html);
    let rows: Vec<_> = document.select(&ROW).collect();
    let mut hits = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let Some(link) = row.select(&LITE_LINK).next() else {
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

        let snippet = rows[index + 1..]
            .iter()
            .take_while(|row| row.select(&LITE_LINK).next().is_none())
            .find_map(|row| row.select(&LITE_SNIPPET).next())
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

fn query_terms(query: &str) -> Vec<String> {
    const STOPWORDS: [&str; 25] = [
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "how", "in", "is", "it",
        "of", "on", "or", "the", "to", "what", "when", "where", "which", "with", "your",
    ];

    query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|term| !STOPWORDS.contains(&term.as_str()))
        .collect()
}

/// Bing fills zero-result pages with unrelated results. Keep only hits that
/// share a meaningful word with the query.
pub(crate) fn keep_relevant(query: &str, hits: Vec<Hit>) -> Vec<Hit> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return hits;
    }

    hits.into_iter()
        .filter(|hit| {
            let haystack = format!("{} {} {}", hit.title, hit.snippet, hit.url).to_lowercase();
            terms.iter().any(|term| haystack.contains(term))
        })
        .collect()
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
    let mut answered = false;

    for base in instances {
        match searx_instance(&base, query, options) {
            Ok(hits) if !hits.is_empty() => return Ok(hits),
            Ok(_) => answered = true,
            Err(message) => failures.push(format!("{base}: {message}")),
        }
    }

    if answered {
        Ok(Vec::new())
    } else {
        Err(failures.join("; "))
    }
}

fn searx_instance(base: &str, query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let base = base.trim_end_matches('/');
    let client = http::client(options.timeout);
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = HashSet::new();
    let mut page = 1;

    while hits.len() < options.limit && page <= 3 {
        let url = format!(
            "{base}/search?q={}&format=json&language=en&safesearch=0&pageno={page}",
            encode(query)
        );
        let body = http::get(&client, &url, "application/json")?.body;
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

    let client = http::client(options.timeout);
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0;

    while hits.len() < options.limit && offset < 10 {
        let count = (options.limit - hits.len()).min(20);
        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={count}&offset={offset}",
            encode(query)
        );
        let body = http::get_with(
            &client,
            &url,
            "application/json",
            &[("X-Subscription-Token", &key)],
        )?
        .body;
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
        let word: String = word
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
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

    fn hit(title: &str, url: &str, snippet: &str) -> Hit {
        Hit {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snippet.to_string(),
            engine: "bing",
        }
    }

    #[test]
    fn bing_url_pins_the_market() {
        let url = bing_url("dashed filename", 11);
        assert!(url.contains("q=dashed+filename"));
        assert!(url.contains("first=11"));
        assert!(url.contains("mkt=en-US"));
        assert!(url.contains("setlang=en"));
        assert!(url.contains("cc=us"));
    }

    #[test]
    fn drops_results_that_do_not_match_the_query() {
        let hits = vec![
            hit(
                "Google Docs",
                "https://docs.google.com/",
                "Create and edit web-based documents.",
            ),
            hit(
                "Dashed Filename in Linux",
                "https://example.com/dashed-filename",
                "Names starting with a dash break commands.",
            ),
        ];

        let hits = keep_relevant("dashed filename", hits);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Dashed Filename in Linux");
    }

    #[test]
    fn keeps_results_matching_any_meaningful_word() {
        let hits = vec![hit(
            "Async Rust",
            "https://example.com/",
            "Write concurrent code.",
        )];
        assert_eq!(keep_relevant("rust async book", hits).len(), 1);
    }

    #[test]
    fn does_not_filter_a_query_made_only_of_stopwords() {
        let hits = vec![hit("Anything", "https://example.com/", "At all.")];
        assert_eq!(keep_relevant("the of to", hits).len(), 1);
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
    fn parses_duckduckgo_lite_results() {
        let html = r#"<html><body><table>
            <tr>
                <td>1.</td>
                <td><a rel="nofollow" href="https://www.rust-lang.org/" class='result-link'>Rust <b>Programming</b></a></td>
            </tr>
            <tr>
                <td></td>
                <td class='result-snippet'>A language <b>empowering</b> everyone.</td>
            </tr>
            <tr>
                <td></td>
                <td><span class='link'>rust-lang.org</span></td>
            </tr>
        </table></body></html>"#;

        let hits = parse_duckduckgo_lite(html);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust Programming");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].snippet, "A language empowering everyone.");
        assert_eq!(hits[0].engine, "ddg");
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
