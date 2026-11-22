use std::collections::HashSet;
use std::process::Command;

use serde::Deserialize;

use crate::engines::{Engine, Hit, Options, encode, keep_relevant};
use crate::read::Page;

const DDG_JS: &str = r#"(()=>{const out=[];for(const r of document.querySelectorAll('div.result')){if(r.classList.contains('result--ad'))continue;const a=r.querySelector('a.result__a');if(!a)continue;let href=a.href;try{const q=new URL(href).searchParams.get('uddg');if(q)href=q}catch(e){}const title=(a.textContent||'').trim();if(!title||!href)continue;const s=r.querySelector('.result__snippet');out.push({title:title,url:href,snippet:s?(s.textContent||'').trim():''})}const body=document.body?document.body.textContent:'';return JSON.stringify({blocked:!!document.querySelector('#anomaly-modal')||/bots use DuckDuckGo|Unfortunately, bots/.test(body||''),results:out})})()"#;

const BING_JS: &str = r#"(()=>{const out=[];for(const li of document.querySelectorAll('li.b_algo')){const a=li.querySelector('h2 a');if(!a)continue;let href=a.href;try{const u=new URL(href).searchParams.get('u');if(u&&u.indexOf('a1')===0)href=atob(u.slice(2))}catch(e){}const h=li.querySelector('h2');const p=li.querySelector('.b_caption p')||li.querySelector('.b_snippet p')||li.querySelector('p');const title=(h?h.textContent:a.textContent||'').trim();if(!title||!href)continue;out.push({title:title,url:href,snippet:p?(p.textContent||'').trim():''})}return JSON.stringify({blocked:/captcha|unusual traffic/i.test(document.title||''),results:out})})()"#;

/// Prefer a semantic article container, drop chrome the reader does not need,
/// and put a line break between block elements so the text keeps its shape.
const READ_JS: &str = r#"(()=>{const pick=document.querySelector('article')||document.querySelector('main')||document.querySelector('[role=main]')||document.body;const c=pick.cloneNode(true);c.querySelectorAll('script,style,noscript,nav,header,footer,aside,form,button,svg,iframe').forEach(e=>e.remove());c.querySelectorAll('p,div,li,h1,h2,h3,h4,h5,h6,tr,br,pre,blockquote,section,article,figcaption').forEach(e=>e.append('\n'));const text=(c.textContent||'').replace(/[ \t]+\n/g,'\n').replace(/\n{3,}/g,'\n\n').replace(/[ \t]{2,}/g,' ').trim();return JSON.stringify({title:document.title,text:text})})()"#;

#[derive(Deserialize)]
struct Payload {
    #[serde(default)]
    blocked: bool,
    #[serde(default)]
    results: Vec<RawHit>,
}

#[derive(Deserialize)]
struct RawHit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    snippet: String,
}

#[derive(Deserialize)]
struct RawPage {
    #[serde(default)]
    title: String,
    #[serde(default)]
    text: String,
}

pub fn search(engine: Engine, query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    match engine {
        Engine::Auto => search_auto(query, options),
        Engine::Bing => bing(query, options),
        Engine::DuckDuckGo => duckduckgo(query, options),
        Engine::Searx | Engine::Brave => Err(format!(
            "{} is not available through the obscura backend; use --engine auto, ddg, or bing",
            engine.name()
        )),
    }
}

fn search_auto(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let mut failures = Vec::new();
    let mut empty = Vec::new();

    for engine in [Engine::DuckDuckGo, Engine::Bing] {
        if options.verbose {
            eprintln!("web-search: trying {}", engine.name());
        }
        match search(engine, query, options) {
            Ok(hits) if !hits.is_empty() => return Ok(hits),
            Ok(_) => {
                if options.verbose {
                    eprintln!("web-search: {}: no results", engine.name());
                }
                empty.push(engine.name());
            }
            Err(message) => {
                if options.verbose {
                    eprintln!("web-search: {}: {message}", engine.name());
                }
                failures.push(format!("{}: {message}", engine.name()));
            }
        }
    }

    if !empty.is_empty() {
        failures.push(format!("{}: no results", empty.join(", ")));
    }

    if failures.is_empty() {
        Ok(Vec::new())
    } else {
        Err(failures.join("; "))
    }
}

fn duckduckgo(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}&kl=us-en",
        encode(query)
    );
    let payload = extract(&url, DDG_JS, options)?;

    if payload.blocked {
        return Err(
            "DuckDuckGo is bot-checking this IP; retry with --engine bing or --stealth".to_string(),
        );
    }

    let mut hits = collect(payload.results, "ddg");
    hits.truncate(options.limit);
    Ok(hits)
}

fn bing(query: &str, options: &Options) -> Result<Vec<Hit>, String> {
    let url = format!(
        "https://www.bing.com/search?q={}&mkt=en-US&setlang=en&cc=us",
        encode(query)
    );
    let payload = extract(&url, BING_JS, options)?;

    if payload.blocked {
        return Err("Bing served a CAPTCHA or an unusual-traffic page".to_string());
    }

    let mut hits = keep_relevant(query, collect(payload.results, "bing"));
    hits.truncate(options.limit);
    Ok(hits)
}

pub fn read_page(options: &Options, url: &str, width: usize) -> Result<Page, String> {
    let json = fetch(options, url, READ_JS, false)?;
    let page: RawPage =
        serde_json::from_str(&json).map_err(|_| "the page returned unreadable data".to_string())?;
    let text = reflow(&page.text, width);

    if text.trim().is_empty() {
        return Err("the page has no readable text".to_string());
    }

    Ok(Page {
        title: clean(&page.title),
        text,
        via: "obscura",
    })
}

fn extract(url: &str, eval: &str, options: &Options) -> Result<Payload, String> {
    let json = fetch(options, url, eval, true)?;
    serde_json::from_str(&json).map_err(|_| "the page returned unreadable data".to_string())
}

fn fetch(options: &Options, url: &str, eval: &str, fast: bool) -> Result<String, String> {
    let mut command = Command::new(&options.obscura);
    command
        .arg("fetch")
        .arg(url)
        .arg("--quiet")
        .arg("--timeout")
        .arg(options.timeout.as_secs().to_string());

    if options.stealth {
        command.arg("--stealth");
    }
    if fast {
        // Search pages are server-rendered, so the adaptive post-load settle
        // only adds latency.
        command.args(["--wait-until", "domcontentloaded", "--wait", "0"]);
    }
    command.arg("--eval").arg(eval);

    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "could not run {:?}; install Obscura from https://github.com/h4ckf0r0day/obscura or pass --obscura <PATH>",
                options.obscura
            )
        } else {
            format!("could not run {:?}: {error}", options.obscura)
        }
    })?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        if message.is_empty() {
            return Err(format!("obscura exited with {}", output.status));
        }
        return Err(message.to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Err("obscura returned no output".to_string());
    }
    Ok(stdout.to_string())
}

fn collect(raw: Vec<RawHit>, engine: &'static str) -> Vec<Hit> {
    let mut seen = HashSet::new();
    let mut hits = Vec::new();

    for hit in raw {
        let title = clean(&hit.title);
        let url = hit.url.trim().to_string();
        let snippet = clean_snippet(&hit.snippet);

        if title.is_empty() || !is_web_url(&url) {
            continue;
        }
        if !seen.insert(url.clone()) {
            continue;
        }

        hits.push(Hit {
            title,
            url,
            snippet,
            engine,
        });
    }

    hits
}

fn is_web_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Bing appends its own "Read more" link to the snippet text.
fn clean_snippet(text: &str) -> String {
    let mut snippet = clean(text);
    while snippet.to_lowercase().ends_with("read more") {
        snippet.truncate(snippet.len() - "read more".len());
        snippet = snippet.trim_end().to_string();
    }
    snippet
}

/// Page text arrives with paragraph breaks but long lines, so re-flow it the
/// same way the HTTP reader does.
fn reflow(text: &str, width: usize) -> String {
    let mut lines: Vec<String> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !lines.last().is_some_and(String::is_empty) {
                lines.push(String::new());
            }
        } else {
            lines.extend(crate::wrap(line, width));
        }
    }

    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, url: &str, snippet: &str) -> Hit {
        Hit {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snippet.to_string(),
            engine: "bing",
        }
    }

    #[test]
    fn keeps_only_web_urls_and_drops_duplicates() {
        let hits = collect(
            vec![
                RawHit {
                    title: "One".into(),
                    url: "https://example.com/".into(),
                    snippet: "first".into(),
                },
                RawHit {
                    title: "Duplicate".into(),
                    url: "https://example.com/".into(),
                    snippet: String::new(),
                },
                RawHit {
                    title: "Mail".into(),
                    url: "mailto:x@example.com".into(),
                    snippet: String::new(),
                },
                RawHit {
                    title: String::new(),
                    url: "https://empty.example/".into(),
                    snippet: String::new(),
                },
            ],
            "bing",
        );

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "One");
    }

    #[test]
    fn drops_bings_read_more_link() {
        assert_eq!(clean_snippet("A snippet. …Read more"), "A snippet. …");
        assert_eq!(clean_snippet("Read more"), "");
        assert_eq!(
            clean_snippet("More reading is good"),
            "More reading is good"
        );
    }

    #[test]
    fn reflow_keeps_paragraphs_and_wraps_long_lines() {
        assert_eq!(
            reflow("one two three four\n\nfive", 9),
            "one two\nthree\nfour\n\nfive"
        );
        assert_eq!(reflow("\n\n", 80), "");
    }

    #[test]
    fn filters_bing_results_that_do_not_match_the_query() {
        let hits = vec![
            hit(
                "Google Docs",
                "https://docs.google.com/",
                "Create and edit documents.",
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
}
