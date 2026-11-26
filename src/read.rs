use std::time::Duration;

use dom_smoothie::{Config, Readability, TextMode};
use scraper::{ElementRef, Html, Node};

use crate::http;

/// Below this many characters a page is treated as an empty shell and retried
/// through the reader proxy. Shell pages (client-rendered apps) return a
/// document with navigation and no article.
const MIN_LOCAL_CHARS: usize = 200;

pub struct ReadOptions {
    pub timeout: Duration,
    pub width: usize,
    pub reader_fallback: bool,
}

pub struct Page {
    pub title: String,
    pub text: String,
    /// Where the text came from: `readability`, `reader`, or `raw`.
    pub via: &'static str,
}

pub fn read(url: &str, options: &ReadOptions) -> Result<Page, String> {
    let client = http::client(options.timeout);
    let fetched = http::get(
        &client,
        url,
        "text/html,application/xhtml+xml;q=0.9,text/plain;q=0.5,*/*;q=0.1",
    )?;

    if !is_html(&fetched.content_type, &fetched.body) {
        return read_other(&fetched);
    }

    let local = extract(&fetched.body, &fetched.url, options.width);

    if matches!(&local, Ok(page) if page.text.chars().count() >= MIN_LOCAL_CHARS) {
        return local;
    }

    if !options.reader_fallback {
        return local.and_then(require_text).map_err(|error| {
            format!(
                "{error}; the page may need JavaScript (drop --no-reader to retry through r.jina.ai)"
            )
        });
    }
    if !reader_allowed(&fetched.url) {
        return local.and_then(require_text).map_err(|error| {
            format!("{error}; reader fallback is disabled for private or credentialed URLs")
        });
    }

    match reader(&client, &fetched.url) {
        Ok(page) if page.text.chars().count() >= MIN_LOCAL_CHARS => Ok(page),
        Ok(fallback) => match local {
            Ok(page) if !page.text.trim().is_empty() => Ok(page),
            _ => Ok(fallback),
        },
        Err(reader_error) => match local {
            Ok(page) if !page.text.trim().is_empty() => Ok(page),
            Ok(_) => Err(format!(
                "the page has no readable text; reader fallback failed: {reader_error}"
            )),
            Err(error) => Err(format!("{error}; reader fallback failed: {reader_error}")),
        },
    }
}

fn require_text(page: Page) -> Result<Page, String> {
    if page.text.trim().is_empty() {
        Err("the page has no readable text".to_string())
    } else {
        Ok(page)
    }
}

fn is_html(content_type: &str, body: &str) -> bool {
    let content_type = content_type.to_lowercase();
    if content_type.contains("html") {
        return true;
    }
    if content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("pdf")
        || content_type.starts_with("image/")
        || content_type.starts_with("audio/")
        || content_type.starts_with("video/")
    {
        return false;
    }
    body.trim_start().starts_with('<')
}

fn read_other(fetched: &http::Fetched) -> Result<Page, String> {
    let content_type = fetched.content_type.to_lowercase();
    if content_type.contains("pdf")
        || content_type.contains("octet-stream")
        || content_type.starts_with("image/")
    {
        let kind = content_type
            .split(';')
            .next()
            .unwrap_or(&content_type)
            .trim();
        return Err(format!("cannot read {kind} in the terminal"));
    }

    let text = tidy(&fetched.body);
    if text.is_empty() {
        return Err("the response has no text".to_string());
    }

    Ok(Page {
        title: String::new(),
        text,
        via: "raw",
    })
}

fn extract(html: &str, url: &str, width: usize) -> Result<Page, String> {
    let config = Config {
        text_mode: TextMode::Formatted,
        ..Config::default()
    };
    let mut readability = Readability::new(html, Some(url), Some(config))
        .map_err(|error| format!("could not parse the page: {error}"))?;
    let article = readability
        .parse()
        .map_err(|error| format!("could not extract the page: {error}"))?;

    Ok(Page {
        title: tidy_line(&article.title),
        text: render(&article.content, width),
        via: "readability",
    })
}

fn reader_allowed(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.to_ascii_lowercase();
            domain.contains('.')
                && !domain.ends_with(".localhost")
                && !domain.ends_with(".local")
                && !domain.ends_with(".internal")
                && !domain.ends_with(".lan")
                && !domain.ends_with(".home")
        }
        Some(url::Host::Ipv4(address)) => {
            !(address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_broadcast())
        }
        Some(url::Host::Ipv6(address)) => {
            !(address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local())
        }
        None => false,
    }
}

/// The reader proxy renders client-side pages that return an empty shell to
/// ordinary HTTP clients.
fn reader(client: &ureq::Agent, url: &str) -> Result<Page, String> {
    if !reader_allowed(url) {
        return Err("refusing to send a private or credentialed URL to the reader proxy".into());
    }
    let target = format!("https://r.jina.ai/{url}");
    let fetched = http::get_with(
        client,
        &target,
        "text/plain",
        &[("X-Return-Format", "text")],
    )?;
    let text = tidy(&fetched.body);

    if text.is_empty() {
        return Err("the reader proxy returned no text".to_string());
    }

    Ok(Page {
        title: String::new(),
        text,
        via: "reader",
    })
}

/// Re-flows the extracted article HTML: prose wraps to the terminal width,
/// block elements become paragraphs, and preformatted blocks stay untouched.
fn render(html: &str, width: usize) -> String {
    let document = Html::parse_document(html);
    let mut renderer = Renderer::new(width);

    walk(document.root_element(), &mut renderer);
    renderer.finish()
}

fn walk(parent: ElementRef, renderer: &mut Renderer) {
    for child in parent.children() {
        match child.value() {
            Node::Text(text) => renderer.add_text(&text.text),
            Node::Element(_) => {
                let Some(element) = ElementRef::wrap(child) else {
                    continue;
                };
                walk_element(element, renderer);
            }
            _ => {}
        }
    }
}

fn walk_element(element: ElementRef, renderer: &mut Renderer) {
    let name = element.value().name().to_ascii_lowercase();

    if is_skipped(&name) {
        return;
    }

    match name.as_str() {
        "pre" => {
            renderer.end_block();
            renderer.verbatim(&element.text().collect::<String>());
            renderer.end_block();
        }
        "br" => renderer.end_line(),
        "li" => {
            renderer.end_line();
            renderer.start_list_item();
            walk(element, renderer);
            renderer.end_line();
        }
        "ul" | "ol" => {
            renderer.end_line();
            renderer.list_depth += 1;
            walk(element, renderer);
            renderer.list_depth -= 1;
            renderer.end_line();
        }
        "td" | "th" => {
            renderer.separate_cell();
            walk(element, renderer);
        }
        "tr" => {
            renderer.end_line();
            walk(element, renderer);
            renderer.end_line();
        }
        _ if is_block(&name) => {
            renderer.end_block();
            walk(element, renderer);
            renderer.end_block();
        }
        _ => walk(element, renderer),
    }
}

fn is_skipped(name: &str) -> bool {
    matches!(
        name,
        "head"
            | "title"
            | "meta"
            | "link"
            | "script"
            | "style"
            | "noscript"
            | "template"
            | "svg"
            | "canvas"
            | "iframe"
            | "object"
            | "embed"
            | "form"
            | "button"
            | "input"
            | "select"
            | "textarea"
            | "dialog"
            | "nav"
            | "aside"
    )
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "dl"
            | "dt"
            | "dd"
            | "figure"
            | "figcaption"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "hr"
    )
}

struct Renderer {
    lines: Vec<String>,
    current: String,
    prefix: String,
    width: usize,
    list_depth: usize,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            lines: Vec::new(),
            current: String::new(),
            prefix: String::new(),
            width: width.max(20),
            list_depth: 0,
        }
    }

    fn add_text(&mut self, text: &str) {
        self.current.push_str(text);
    }

    fn start_list_item(&mut self) {
        self.prefix = format!("{}- ", "  ".repeat(self.list_depth.saturating_sub(1)));
    }

    fn separate_cell(&mut self) {
        if !self.current.trim().is_empty() {
            self.current.push_str("  ");
        }
    }

    fn end_line(&mut self) {
        self.flush();
    }

    fn end_block(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

    fn verbatim(&mut self, text: &str) {
        self.flush();
        for line in text.lines() {
            self.lines.push(line.trim_end().to_string());
        }
    }

    /// Collapses whitespace inside the current block and wraps it. Text from
    /// adjacent inline elements is joined without inventing spaces, so
    /// `<a>error</a>,` stays "error,".
    fn flush(&mut self) {
        let text = self
            .current
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        self.current.clear();

        if text.is_empty() {
            self.prefix.clear();
            return;
        }

        let prefix = std::mem::take(&mut self.prefix);
        let indent = prefix.chars().count();
        let continuation = " ".repeat(indent);

        for (index, line) in crate::wrap(&text, self.width.saturating_sub(indent))
            .into_iter()
            .enumerate()
        {
            if index == 0 {
                self.lines.push(format!("{prefix}{line}"));
            } else {
                self.lines.push(format!("{continuation}{line}"));
            }
        }
    }

    fn finish(mut self) -> String {
        self.end_block();
        while self.lines.last().is_some_and(|line| line.is_empty()) {
            self.lines.pop();
        }
        self.lines.join("\n")
    }
}

fn tidy_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Trims trailing spaces and collapses runs of blank lines, without touching
/// indentation inside lines.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end().to_string();
        if line.is_empty() && lines.last().is_some_and(|last| last.is_empty()) {
            continue;
        }
        lines.push(line);
    }

    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = r#"<!doctype html>
<html><head>
  <title>Understanding lifetimes</title>
  <script>window.tracker = 1;</script>
</head>
<body>
  <nav><a href="/">Home</a> <a href="/about">About</a></nav>
  <main>
    <article>
      <h1>Understanding lifetimes</h1>
      <p>Lifetimes describe how long a reference is valid. They are the part of
      the borrow checker that beginners find hardest, but the rules are small.</p>
      <p>A lifetime is a name for a region of code. The compiler uses it to
      prove that every reference outlives the data it points at.</p>
      <pre><code>fn longest&lt;'a&gt;(x: &amp;'a str, y: &amp;'a str) -&gt; &amp;'a str {
    x
}</code></pre>
      <p>Most of the time lifetimes are inferred, so you never write them.</p>
      <ul>
        <li>Every reference has a lifetime.</li>
        <li>The compiler checks the smallest region that works.</li>
      </ul>
    </article>
  </main>
  <footer>Copyright nobody</footer>
</body></html>"#;

    #[test]
    fn extracts_the_article_and_drops_the_chrome() {
        let page = extract(ARTICLE, "https://example.com/lifetimes", 60).unwrap();

        assert_eq!(page.title, "Understanding lifetimes");
        assert_eq!(page.via, "readability");
        assert!(page.text.contains("Lifetimes describe how long"));
        assert!(page.text.contains("- Every reference has a lifetime."));
        assert!(!page.text.contains("Copyright"));
        assert!(!page.text.contains("Home"));
        assert!(!page.text.contains("tracker"));
    }

    #[test]
    fn reflows_prose_but_keeps_code_blocks() {
        let page = extract(ARTICLE, "https://example.com/lifetimes", 40).unwrap();

        assert!(
            page.text
                .contains("fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {")
        );
        assert!(page.text.contains("\n    x\n"));

        let prose: Vec<&str> = page
            .text
            .lines()
            .filter(|line| {
                !line.starts_with("    ") && !line.starts_with("fn longest") && *line != "}"
            })
            .collect();
        assert!(!prose.is_empty());
        assert!(prose.iter().all(|line| line.chars().count() <= 40));
    }

    #[test]
    fn reports_a_page_without_readable_text() {
        // A client-rendered shell: no article to extract. `read` turns this into
        // a reader-proxy attempt, or an error when the proxy is disabled.
        let html =
            r#"<html><head><title>App</title></head><body><div id="root"></div></body></html>"#;
        let page = extract(html, "https://example.com/app", 80);
        assert!(page.is_err() || page.unwrap().text.is_empty());
    }

    #[test]
    fn reader_fallback_does_not_expose_private_urls() {
        assert!(reader_allowed("https://example.com/article?id=1"));
        assert!(!reader_allowed(
            "http://localhost:8080/private?token=secret"
        ));
        assert!(!reader_allowed("http://127.0.0.1/private"));
        assert!(!reader_allowed("http://10.0.0.4/private"));
        assert!(!reader_allowed("http://[::1]/private"));
        assert!(!reader_allowed("http://intranet/private"));
        assert!(!reader_allowed("https://user:password@example.com/private"));
        assert!(!reader_allowed("file:///etc/passwd"));
    }

    #[test]
    fn detects_html_by_content_type_or_sniffing() {
        assert!(is_html("text/html; charset=utf-8", ""));
        assert!(is_html("", "<!doctype html><p>x</p>"));
        assert!(!is_html("application/pdf", "%PDF-1.7"));
        assert!(!is_html("application/json", "{\"a\":1}"));
        assert!(!is_html("text/plain", "just words"));
    }

    #[test]
    fn tidies_blank_lines_without_touching_indentation() {
        assert_eq!(tidy("\n\na  \n\n\n\n   b\n\n"), "a\n\n   b");
        assert_eq!(tidy(""), "");
    }

    #[test]
    fn wraps_words_at_the_width() {
        let mut renderer = Renderer::new(20);
        renderer.add_text("one two three four five");
        assert_eq!(renderer.finish(), "one two three four\nfive");
    }

    #[test]
    fn keeps_inline_text_joined_without_extra_spaces() {
        let mut renderer = Renderer::new(80);
        renderer.add_text("A recoverable ");
        renderer.add_text("error");
        renderer.add_text(", such as a missing file.");
        assert_eq!(
            renderer.finish(),
            "A recoverable error, such as a missing file."
        );
    }
}
