mod engines;

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
    query: String,
    limit: usize,
    engine: Engine,
    json: bool,
    urls: bool,
    timeout: Duration,
    searx_url: Option<String>,
    verbose: bool,
}

fn usage(code: i32) -> ! {
    let text = format!(
        "Search the web from the terminal

usage: web-search [options] <query...>

examples:
  web-search rust async book
  web-search -n 5 --urls \"rust 2024 edition\"
  echo \"who invented unix\" | web-search

engines:
  auto    Try ddg, then bing, then searx (default)
  ddg     DuckDuckGo HTML results, no key needed
  bing    Bing web results, no key needed
  searx   SearXNG instance from --searx-url or SEARX_URL
  brave   Brave Search API, needs BRAVE_API_KEY

options:
  -n, --limit <n>       Number of results (default: {DEFAULT_LIMIT}, max: {MAX_LIMIT})
  -e, --engine <name>   Search engine: {names}
      --searx-url <url> SearXNG base URL (default: public instances)
      --json            Print results as JSON
      --urls            Print only the result URLs
  -t, --timeout <secs>  Request timeout in seconds (default: {DEFAULT_TIMEOUT_SECS})
  -v, --verbose         Print engine and timing to stderr
  -h, --help            Show this help
  -V, --version         Show the version
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
        query: String::new(),
        limit: DEFAULT_LIMIT,
        engine: Engine::Auto,
        json: false,
        urls: false,
        timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        searx_url: None,
        verbose: false,
    };
    let mut words = Vec::new();
    let mut argv = arguments.into_iter();

    while let Some(arg) = argv.next() {
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

    args.query = read_query(words);
    args
}

/// A query comes from the arguments, or from stdin when it is piped in. There is
/// never a prompt: without either, this is a usage error.
fn read_query(words: Vec<String>) -> String {
    let query = words.join(" ");
    if !query.trim().is_empty() {
        return query;
    }

    let mut input = String::new();
    if !io::stdin().is_terminal() && io::stdin().read_to_string(&mut input).is_ok() {
        let piped = input.trim();
        if !piped.is_empty() {
            return piped.to_string();
        }
    }

    usage(2);
}

fn terminal_width() -> usize {
    env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width >= 20)
        .unwrap_or(DEFAULT_WIDTH)
}

fn colors_enabled() -> bool {
    io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none()
}

fn paint(text: &str, attributes: &str) -> String {
    if colors_enabled() {
        format!("\x1b[{attributes}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
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
        writeln!(out, "{} {}", paint(&number, "2"), paint(&hit.title, "1"))?;
        writeln!(out, "   {}", paint(&hit.url, "2"))?;

        for line in wrap(&hit.snippet, width.saturating_sub(3)) {
            writeln!(out, "   {line}")?;
        }
    }

    Ok(())
}

fn main() {
    let args = parse_args();
    let options = Options {
        limit: args.limit,
        timeout: args.timeout,
        searx_url: args.searx_url.clone(),
    };

    let started = Instant::now();
    let hits = match engines::search(args.engine, &args.query, &options) {
        Ok(hits) => hits,
        Err(message) => {
            eprintln!("web-search: {message}");
            process::exit(1);
        }
    };

    if args.verbose {
        let engine = hits.first().map_or(args.engine.name(), |hit| hit.engine);
        eprintln!(
            "web-search: {engine}: {} results in {} ms",
            hits.len(),
            started.elapsed().as_millis()
        );
    }

    if hits.is_empty() && !args.json && !args.urls {
        eprintln!("web-search: no results");
        return;
    }

    if let Err(error) = print_results(&hits, &args) {
        if error.kind() == io::ErrorKind::BrokenPipe {
            return;
        }
        eprintln!("web-search: could not write output: {error}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_query_words() {
        let args = parse_args_from(vec!["rust".into(), "async".into(), "book".into()]);
        assert_eq!(args.query, "rust async book");
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
        assert_eq!(args.query, "query");
    }

    #[test]
    fn a_lone_dash_is_part_of_the_query() {
        let args = parse_args_from(vec!["--json".into(), "-".into(), "x".into()]);
        assert!(args.json);
        assert_eq!(args.query, "- x");
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
