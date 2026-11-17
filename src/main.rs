mod cache;
mod engines;
mod http;
mod read;

use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::process;
use std::time::{Duration, Instant};

use engines::{Engine, Hit, Options};
use serde_json::json;

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
const DEFAULT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_WIDTH: usize = 80;

struct Args {
    query: Option<String>,
    limit: usize,
    engine: Engine,
    json: bool,
    urls: bool,
    read: Option<String>,
    reader_fallback: bool,
    timeout: Duration,
    searx_url: Option<String>,
    verbose: bool,
}

fn usage(code: i32) -> ! {
    let text = format!(
        "A Rust CLI for web search and reading

Usage: web-search [OPTIONS] <QUERY>...
       web-search -r <N|URL> [QUERY]...

Examples:
  web-search rust async book
  web-search -n 5 --urls \"rust 2024 edition\"
  web-search -r 2 rust async book
  web-search -r 2                    # result 2 of the previous search
  web-search -r https://example.com/
  echo \"who invented unix\" | web-search

Engines:
  auto    Try ddg, then bing, then a configured searx (default)
  ddg     DuckDuckGo HTML results, no key needed
  bing    Bing web results, no key needed
  searx   SearXNG instance from --searx-url or SEARX_URL
  brave   Brave Search API, needs BRAVE_API_KEY

Options:
  -n, --limit <N>       Number of results (default: {DEFAULT_LIMIT}, max: {MAX_LIMIT})
  -e, --engine <NAME>   Search engine: {names}
  -r, --read <N|URL>    Print result n, or the URL, as plain text
      --no-reader       Do not retry client-side pages through r.jina.ai
      --searx-url <URL> SearXNG base URL (default: public instances)
      --json            Print results as JSON
      --urls            Print only the result URLs
  -t, --timeout <SECS>  Request timeout in seconds (default: {DEFAULT_TIMEOUT_SECS})
  -v, --verbose         Print engine and timing to stderr
  -h, --help            Print help
  -V, --version         Print version
",
        names = Engine::NAMES
    );

    if code == 0 {
        print!("{text}");
    } else {
        eprint!("{text}");
    }
    process::exit(code);
}

fn parse_args() -> Args {
    let arguments: Vec<String> = env::args().skip(1).collect();
    parse_args_from(arguments)
}

fn parse_args_from(arguments: Vec<String>) -> Args {
    let mut args = Args {
        query: None,
        limit: DEFAULT_LIMIT,
        engine: Engine::Auto,
        json: false,
        urls: false,
        read: None,
        reader_fallback: true,
        timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        searx_url: None,
        verbose: false,
    };
    let mut words = Vec::new();
    let mut argv = arguments.into_iter();
    let mut end_of_options = false;

    while let Some(arg) = argv.next() {
        if end_of_options {
            words.push(arg);
            continue;
        }
        if arg == "--" {
            end_of_options = true;
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };

        let mut take_value = |name: &str| -> String {
            inline.clone().unwrap_or_else(|| {
                argv.next().unwrap_or_else(|| {
                    eprintln!("error: {name} needs a value");
                    usage(2);
                })
            })
        };

        match flag.as_str() {
            "-h" | "--help" => usage(0),
            "-V" | "--version" => {
                println!("web-search {}", env!("CARGO_PKG_VERSION"));
                process::exit(0);
            }
            "-n" | "--limit" => {
                let value = take_value("--limit");
                args.limit = value.parse().unwrap_or_else(|_| {
                    eprintln!("error: --limit must be a number");
                    usage(2);
                });
                if !(1..=MAX_LIMIT).contains(&args.limit) {
                    eprintln!("error: --limit must be between 1 and {MAX_LIMIT}");
                    usage(2);
                }
            }
            "-e" | "--engine" => {
                let value = take_value("--engine");
                args.engine = Engine::parse(&value).unwrap_or_else(|| {
                    eprintln!("error: unknown engine: {value}");
                    eprintln!("available engines: {}", Engine::NAMES);
                    usage(2);
                });
            }
            "-r" | "--read" => {
                let value = take_value("--read");
                if value.trim().is_empty() {
                    eprintln!("error: --read needs a result number or a URL");
                    usage(2);
                }
                args.read = Some(value);
            }
            "--no-reader" => args.reader_fallback = false,
            "--searx-url" => {
                let value = take_value("--searx-url");
                if value.trim().is_empty() {
                    eprintln!("error: --searx-url must not be empty");
                    usage(2);
                }
                args.searx_url = Some(value);
            }
            "-t" | "--timeout" => {
                let value = take_value("--timeout");
                let seconds: u64 = value.parse().unwrap_or_else(|_| {
                    eprintln!("error: --timeout must be a number of seconds");
                    usage(2);
                });
                if seconds == 0 {
                    eprintln!("error: --timeout must be at least 1 second");
                    usage(2);
                }
                args.timeout = Duration::from_secs(seconds);
            }
            "--json" => args.json = true,
            "--urls" => args.urls = true,
            "-v" | "--verbose" => args.verbose = true,
            _ if arg.starts_with('-') && arg != "-" => {
                eprintln!("error: unknown option: {arg}");
                usage(2);
            }
            _ => words.push(arg),
        }
    }

    if args.json && args.urls {
        eprintln!("error: --json and --urls cannot be combined");
        usage(2);
    }
    if args.read.is_some() && (args.json || args.urls) {
        eprintln!("error: --read cannot be combined with --json or --urls");
        usage(2);
    }

    args.query = read_query(words);

    if args.query.is_none() && args.read.is_none() {
        usage(2);
    }

    args
}

/// A query comes from the arguments, or from stdin when it is piped in. There is
/// never a prompt: reading a cached result needs no query at all.
fn read_query(words: Vec<String>) -> Option<String> {
    let query = words.join(" ");
    if !query.trim().is_empty() {
        return Some(query);
    }

    let mut input = String::new();
    if !io::stdin().is_terminal() && io::stdin().read_to_string(&mut input).is_ok() {
        let piped = input.trim();
        if !piped.is_empty() {
            return Some(piped.to_string());
        }
    }

    None
}

fn terminal_width() -> usize {
    env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width >= 20)
        .unwrap_or(DEFAULT_WIDTH)
}

pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();

    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }

    if !line.is_empty() {
        lines.push(line);
    }

    lines
}

fn print_results(hits: &[Hit], args: &Args) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if args.json {
        let results: Vec<_> = hits
            .iter()
            .map(|hit| {
                json!({
                    "title": hit.title,
                    "url": hit.url,
                    "snippet": hit.snippet,
                    "engine": hit.engine,
                })
            })
            .collect();
        writeln!(out, "{}", serde_json::to_string_pretty(&results)?)?;
        return Ok(());
    }

    if args.urls {
        for hit in hits {
            writeln!(out, "{}", hit.url)?;
        }
        return Ok(());
    }

    let width = terminal_width();
    let number_width = hits.len().max(1).to_string().len();

    for (index, hit) in hits.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }

        let number = format!("{:>number_width$}.", index + 1);
        writeln!(out, "{number} {}", hit.title)?;
        writeln!(out, "   {}", hit.url)?;

        for line in wrap(&hit.snippet, width.saturating_sub(3)) {
            writeln!(out, "   {line}")?;
        }
    }

    Ok(())
}

fn print_page(title: &str, text: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if !title.is_empty() {
        writeln!(out, "{title}")?;
        writeln!(out)?;
    }

    writeln!(out, "{}", text.trim_end())
}

fn main() {
    let args = parse_args();
    let options = Options {
        limit: args.limit,
        timeout: args.timeout,
        searx_url: args.searx_url.clone(),
        verbose: args.verbose,
    };

    let result = match args.read.as_deref() {
        Some(target) => read_result(&args, &options, target),
        None => search_result(&args, &options),
    };

    if let Err(message) = result {
        eprintln!("error: {message}");
        process::exit(1);
    }
}

fn search_result(args: &Args, options: &Options) -> Result<(), String> {
    let hits = search(args, options)?;

    if hits.is_empty() && !args.json && !args.urls {
        eprintln!("no results");
        return Ok(());
    }

    ignore_broken_pipe(print_results(&hits, args))
}

fn search(args: &Args, options: &Options) -> Result<Vec<Hit>, String> {
    let Some(query) = args.query.as_deref() else {
        return Err("no query given".to_string());
    };

    let started = Instant::now();
    let hits = engines::search(args.engine, query, options)?;

    if args.verbose {
        let engine = hits.first().map_or(args.engine.name(), |hit| hit.engine);
        eprintln!(
            "web-search: {engine}: {} results in {} ms",
            hits.len(),
            started.elapsed().as_millis()
        );
    }

    cache::save(query, &hits);
    Ok(hits)
}

fn read_result(args: &Args, options: &Options, target: &str) -> Result<(), String> {
    let (url, known_title) = if is_url(target) {
        (target.to_string(), None)
    } else {
        let index: usize = target
            .parse()
            .map_err(|_| format!("{target:?} is neither a result number nor an http(s) URL"))?;
        if index == 0 {
            return Err("result numbers start at 1".to_string());
        }

        let entries: Vec<cache::Entry> = if args.query.is_some() {
            search(args, options)?
                .into_iter()
                .map(|hit| cache::Entry {
                    title: hit.title,
                    url: hit.url,
                })
                .collect()
        } else {
            cache::load()
                .ok_or_else(|| {
                    "there is no previous search to pick from; pass a query or a URL".to_string()
                })?
                .results
        };

        let entry = entries.get(index - 1).ok_or_else(|| {
            format!(
                "there is no result {index}; the last search had {}",
                entries.len()
            )
        })?;
        (entry.url.clone(), Some(entry.title.clone()))
    };

    let started = Instant::now();
    let read_options = read::ReadOptions {
        timeout: args.timeout,
        width: terminal_width(),
        reader_fallback: args.reader_fallback,
    };
    let page = read::read(&url, &read_options).map_err(|message| format!("{url}: {message}"))?;

    if args.verbose {
        eprintln!(
            "web-search: {}: {} chars in {} ms",
            page.via,
            page.text.chars().count(),
            started.elapsed().as_millis()
        );
    }

    let title = if page.title.is_empty() {
        known_title.unwrap_or_default()
    } else {
        page.title
    };

    ignore_broken_pipe(print_page(&title, &page.text))
}

fn is_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

fn ignore_broken_pipe(result: io::Result<()>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(format!("could not write output: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_query_words() {
        let args = parse_args_from(vec!["rust".into(), "async".into(), "book".into()]);
        assert_eq!(args.query.as_deref(), Some("rust async book"));
        assert_eq!(args.limit, DEFAULT_LIMIT);
        assert_eq!(args.engine, Engine::Auto);
    }

    #[test]
    fn accepts_inline_and_separate_option_values() {
        let args = parse_args_from(vec![
            "--limit=3".into(),
            "-e".into(),
            "ddg".into(),
            "--timeout=5".into(),
            "query".into(),
        ]);
        assert_eq!(args.limit, 3);
        assert_eq!(args.engine, Engine::DuckDuckGo);
        assert_eq!(args.timeout, Duration::from_secs(5));
        assert_eq!(args.query.as_deref(), Some("query"));
    }

    #[test]
    fn a_lone_dash_is_part_of_the_query() {
        let args = parse_args_from(vec!["--json".into(), "-".into(), "x".into()]);
        assert!(args.json);
        assert_eq!(args.query.as_deref(), Some("- x"));
    }

    #[test]
    fn double_dash_ends_option_parsing() {
        let args = parse_args_from(vec!["-n".into(), "2".into(), "--".into(), "-n".into()]);
        assert_eq!(args.limit, 2);
        assert_eq!(args.query.as_deref(), Some("-n"));
    }

    #[test]
    fn a_result_can_be_read_without_a_query() {
        let args = parse_args_from(vec!["-r".into(), "2".into()]);
        assert_eq!(args.read.as_deref(), Some("2"));
        assert_eq!(args.query, None);
    }

    #[test]
    fn a_read_target_can_be_a_url() {
        let args = parse_args_from(vec!["--read=https://example.com/".into()]);
        assert!(is_url(args.read.as_deref().unwrap()));
        assert!(args.reader_fallback);
    }

    #[test]
    fn the_reader_fallback_can_be_disabled() {
        let args = parse_args_from(vec!["--no-reader".into(), "-r".into(), "1".into()]);
        assert!(!args.reader_fallback);
    }

    #[test]
    fn wraps_to_the_requested_width() {
        assert_eq!(
            wrap("one two three four", 9),
            vec!["one two", "three", "four"]
        );
        assert_eq!(wrap("", 80), Vec::<String>::new());
    }
}
