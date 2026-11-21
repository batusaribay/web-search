# web-search

A Rust CLI to search the web

![web-search in a terminal](preview.png)

## Why this exists

Search results live in a browser, wrapped in ads and scripts. This tool asks an
engine, normalizes the answers, and prints titles, URLs, and snippets as plain
text that pipes into `grep`, `jq`, or a reader.

## How it works

The query comes from the arguments or from piped stdin. Each engine is fetched
over TLS with `ureq` and `rustls`, a request timeout, and an 8 MiB response cap.
DuckDuckGo HTML, Bing RSS, SearXNG JSON, and the Brave API are normalized into
one result shape. Titles and snippets are cleaned and wrapped to `COLUMNS`, or
80 when it is unset.

`--read` fetches the selected page once and extracts article text with
`dom_smoothie`. If extraction is too small, it can retry through `r.jina.ai`
unless `--no-reader` is set.

## Usage

```text
web-search [OPTIONS] <QUERY>...

Options:
  -n, --limit <N>       Number of results (default: 10, max: 50)
  -e, --engine <NAME>   Search engine: auto, bing, ddg, searx, brave
  -r, --read <N|URL>    Print result n, or the URL, as plain text
      --no-reader       Do not retry client-side pages through r.jina.ai
      --searx-url <URL> SearXNG base URL
      --json            Print results as JSON
      --urls            Print only the result URLs
      --color <WHEN>    Color output: auto, always, or never [default: auto]
  -t, --timeout <SECS>  Request timeout in seconds (default: 15)
  -v, --verbose         Print engine and timing to stderr
  -h, --help            Print help
  -V, --version         Print version
```

Typical calls:

```bash
web-search rust async book
web-search -n 5 --urls "rust 2024 edition"
web-search --json "who invented unix" | jq -r '.[].url'
echo "tauri vs egui" | web-search -n 5
web-search -r 2 rust async book
web-search -r https://example.com/
```

## Notes

`auto` tries DuckDuckGo, then Brave when `BRAVE_API_KEY` is set, then a SearXNG
instance, then Bing. Bing results that share no word with the query are dropped.
Use `-v` to see which engine answered.

The last result list is cached at `$XDG_CACHE_HOME/web-search/last.json`, so
`web-search -r 2` can reopen result 2 of the previous search. Queries are not
logged. Exit codes are `0` for a completed search or read, `1` for a failed
request, and `2` for usage errors.

The HTML endpoints are not APIs and can return bot checks or rate limits. Use
Brave or your own SearXNG instance for regular use. The default SearXNG
instance sees your queries; point `--searx-url` or `SEARX_URL` elsewhere if
that matters.

TLS is handled by `rustls`. Only the selected engine and result URL are
contacted. Response sizes and timeouts are bounded. No credentials, cookies,
configuration, or telemetry are read or sent. Unsafe code is forbidden by the
package lint configuration.

Color uses the terminal palette and ANSI attributes only. Body text inherits
the terminal foreground, while result numbers, titles, and URLs use semantic
roles. `--color never`, `NO_COLOR`, `CLICOLOR=0`, `TERM=dumb`, and non-terminal
output all disable it. JSON and `--urls` are never colored.

## Install

**Requirements:** Rust 1.88+ and `git`.

```bash
cargo install --git https://github.com/batusaribay/web-search.git --locked
```

## License

This project is licensed under the [MIT License](LICENSE).
