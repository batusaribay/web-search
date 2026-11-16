# web-search

Search the web from the terminal

## What it is

A single Rust binary that takes a query and prints ranked results as title, URL,
and snippet. It is not a TUI and not interactive: the query comes from the
arguments or from piped stdin, the results go to stdout, and the process exits.
There is no pager, no prompt, and no config file.

```
$ web-search rust async book
 1. Async Programming in Rust - Rust Cookbook
   https://rust-lang-nursery.github.io/rust-cookbook/concurrency/async.html
   A collection of runnable examples that illustrate how to use async Rust.

 2. Async in depth | Tokio
   https://tokio.rs/tokio/tutorial/async
   Async Rust is a cooperative scheduling model: tasks yield control at await
   points, and a runtime decides what runs next.
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
```

| Option | Meaning |
| --- | --- |
| `-n, --limit <n>` | Number of results, 1 to 50 (default: 10) |
| `-e, --engine <name>` | `auto`, `bing`, `ddg`, `searx`, or `brave` (default: `auto`) |
| `--searx-url <url>` | SearXNG base URL (default: public instances) |
| `--json` | Print an array of `{title, url, snippet, engine}` objects |
| `--urls` | Print only the result URLs, one per line |
| `-t, --timeout <secs>` | Per-request timeout (default: 15) |
| `-v, --verbose` | Print the engine used and the timing to stderr |
| `-h, --help` | Show help |
| `-V, --version` | Show the version |

Exit codes: `0` for a completed search (even with no results), `1` when every
engine request failed, `2` for usage errors.

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
   and a 4 MiB response cap.
3. DuckDuckGo returns HTML, parsed with `scraper`: ad blocks are skipped and
   `duckduckgo.com/l/?uddg=...` links are resolved to the site they point at.
   Bing returns RSS, parsed with `quick-xml` so that entities and CDATA are
   handled correctly. SearXNG and Brave return JSON, read with `serde_json`.
4. Titles and snippets are cleaned once per result: tags stripped, entities
   decoded, whitespace collapsed. Snippets are wrapped to `COLUMNS` (80 by
   default).
5. Output is colourised only when stdout is a terminal and `NO_COLOR` is unset.
   Writing to a closed pipe exits quietly, so `web-search ... | head` works.

The `--limit` value is a ceiling, not a promise: an engine returns whatever it
has. Bing's RSS output usually stops at 10 results per query and ignores the
page offset, so ask for more only with `brave` or a SearXNG instance that pages.

## Limits

The HTML endpoints are not APIs. They can return bot checks or rate limits,
especially from shared or datacentre addresses; that is what `auto` is for. For
regular or high-volume use, use the Brave API or your own SearXNG instance.
Requests are made once per search; nothing is cached or logged.
