# web-search

Search the web from the terminal

## What it is

A single Rust binary that takes a query, prints ranked results as title, URL, and
snippet, and can turn any of those results into the page's text without leaving
the terminal. It is not a TUI and not interactive: input comes from the arguments
or from piped stdin, the output goes to stdout, and the process exits. There is
no pager, no prompt, and no config file.

```
$ web-search rust async book
 1. Async Programming in Rust - Rust Cookbook
   https://rust-lang-nursery.github.io/rust-cookbook/concurrency/async.html
   A collection of runnable examples that illustrate how to use async Rust.

 2. Async in depth | Tokio
   https://tokio.rs/tokio/tutorial/async
   Async Rust is a cooperative scheduling model: tasks yield control at await
   points, and a runtime decides what runs next.

$ web-search -r 2
Async in depth | Tokio

Much of the async runtime is built on top of the futures crate. A future is a
value that represents work that will finish at some point in the future...
```

## Why this exists

Searching from a terminal usually means either a full-screen browser or piping
`curl` output through grep. Both lose the shape of the results. This keeps the
search where the rest of the shell is: one command, ranked output, and flags that
make the output machine-friendly (`--urls`, `--json`).

It never waits for input. If there is no query on the command line and nothing on
stdin, it prints usage and exits non-zero.

## Install

```
cargo install --path .
```

Requires Rust 1.88 or newer. The binary is installed as `web-search`.

## Usage

```
web-search [options] <query...>
```

Queries do not need quotes; every non-option argument is joined with spaces.

```
web-search rust async book
web-search -n 5 --urls "rust 2024 edition"
web-search --json "who invented unix" | jq -r '.[].url'
echo "tauri vs egui" | web-search -n 5
web-search -e brave -n 20 "embedded rust"
web-search -r 2 rust async book
```

| Option | Meaning |
| --- | --- |
| `-n, --limit <n>` | Number of results, 1 to 50 (default: 10) |
| `-e, --engine <name>` | `auto`, `bing`, `ddg`, `searx`, or `brave` (default: `auto`) |
| `-r, --read <n\|url>` | Print result `n`, or the URL, as plain text |
| `--no-reader` | Never retry a page through `r.jina.ai` |
| `--searx-url <url>` | SearXNG base URL (default: public instances) |
| `--json` | Print an array of `{title, url, snippet, engine}` objects |
| `--urls` | Print only the result URLs, one per line |
| `-t, --timeout <secs>` | Per-request timeout (default: 15) |
| `-v, --verbose` | Print the engine used and the timing to stderr |
| `-h, --help` | Show help |
| `-V, --version` | Show the version |

Exit codes: `0` for a completed search or read (even with no results), `1` when a
request failed, `2` for usage errors.

## Reading a result

```
web-search -r 2 rust async book     # search, then print result 2 as text
web-search -r 2                     # result 2 of the previous search
web-search -r https://example.com/  # any http(s) URL
```

The page is fetched once and the article is extracted with `dom_smoothie`, a Rust
port of Readability. Prose is reflowed to `COLUMNS` (80 when unset), paragraphs
become blank-line separated blocks, lists get `-` bullets, and `<pre>` blocks keep
their original spacing so code stays readable. The output is plain text: no
colour, no pager, no typing animation. Pipe it to `less` or redirect it to a file.

The last result list is cached in `$XDG_CACHE_HOME/web-search/last.json`
(`~/.cache/web-search/last.json` when `XDG_CACHE_HOME` is unset). That is what
lets `-r 2` refer to the search you just ran instead of re-running it and hoping
for the same ranking. If there is no cache, the error says so; nothing is
invented.

Client-rendered pages return an empty shell to an HTTP client. When the extracted
text is under 200 characters, the URL is retried through `r.jina.ai`, which
returns reader-mode text; `--no-reader` disables that, keeping every request
between you and the site.

## Engines

| Engine | Source | Notes |
| --- | --- | --- |
| `ddg` | DuckDuckGo's HTML endpoint | Default. No key. Skips ads, resolves redirect links. |
| `bing` | Bing's RSS output | No key. Usually 10 results per query. |
| `searx` | SearXNG's JSON API | Needs `--searx-url` or `SEARX_URL`; falls back to public instances. |
| `brave` | Brave Search API | Needs `BRAVE_API_KEY`. Most reliable, up to 50 results. |

`auto` tries DuckDuckGo, then Bing, then SearXNG, and stops at the first
engine that returns something. Use `-v` to see which one answered.

Private instances are preferred for SearXNG:

```
export SEARX_URL=https://searx.example.org
web-search -e searx "anything"
```

The Brave API key is free-tier and set in the environment:

```
export BRAVE_API_KEY=...
web-search -e brave "anything"
```

## How it works

1. The query is taken from the arguments, or read from stdin when one is piped
   in. Options may be written as `--limit 5` or `--limit=5`.
2. Each engine is fetched over TLS with `ureq` and `rustls`, with a browser user
   agent because the HTML endpoints reject unknown clients, a request timeout,
   and an 8 MiB response cap.
3. DuckDuckGo returns HTML, parsed with `scraper`: ad blocks are skipped and
   `duckduckgo.com/l/?uddg=...` links are resolved to the site they point at.
   Bing returns RSS, parsed with `quick-xml` so that entities and CDATA are
   handled correctly. SearXNG and Brave return JSON, read with `serde_json`.
4. Titles and snippets are cleaned once per result: tags stripped, entities
   decoded, whitespace collapsed. Snippets are wrapped to `COLUMNS` (80 by
   default).
5. Output is colourised only when stdout is a terminal and `NO_COLOR` is unset.
   Writing to a closed pipe exits quietly, so `web-search ... | head` works.
6. `--read` resolves its target: a URL is used directly, a number is looked up in
   the results of the current query or, without a query, in the cached list.
7. The page is fetched, and Readability is run over the HTML. The extracted
   article HTML is walked once more to reflow prose and preserve preformatted
   blocks; a `<pre>` block is printed exactly as it appears in the page.
8. If extraction is too small to be an article, the reader proxy is tried with
   `X-Return-Format: text`. Its answer is printed as-is, minus trailing spaces
   and repeated blank lines.

The `--limit` value is a ceiling, not a promise: an engine returns whatever it
has. Bing's RSS output usually stops at 10 results per query and ignores the
page offset, so ask for more only with `brave` or a SearXNG instance that pages.

## Limits

The HTML endpoints are not APIs. They can return bot checks or rate limits,
especially from shared or datacentre addresses; that is what `auto` is for. For
regular or high-volume use, use the Brave API or your own SearXNG instance.

Reading keeps only the last result list, and only for `--read` to find a number;
queries are not logged anywhere else. The reader fallback sends the page URL to
`r.jina.ai`, so use `--no-reader` if that is not acceptable. JavaScript-only pages
still read poorly even through the proxy, and a PDF is reported as unreadable
rather than dumped as binary.
