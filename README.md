# web-search

A Rust CLI to search the web

![web-search in a terminal](preview.png)

## Why this exists

Search results live in a browser, wrapped in ads and scripts. This tool asks an
engine, normalizes the answers, and prints titles, URLs, and snippets as plain
text that pipes into `grep`, `jq`, or a reader.

The default backend is a small HTTP client. When an engine answers with a bot
check or builds results with JavaScript, `--backend obscura` runs the same
search through a real headless browser instead.

## How it works

The query comes from the arguments or from piped stdin. Each engine is fetched
over TLS with `ureq` and `rustls`, a request timeout, and an 8 MiB response cap.
DuckDuckGo HTML, Bing RSS, SearXNG JSON, and the Brave API are normalized into
one result shape. Titles and snippets are cleaned and wrapped to `COLUMNS`, or
80 when it is unset.

`--read` fetches the selected page once and extracts article text with
`dom_smoothie`. If extraction is too small, it can retry through `r.jina.ai`
unless `--no-reader` is set.

`--backend obscura` shells out to `obscura fetch` and asks the rendered page for
its results with `--eval`. DuckDuckGo and Bing are supported; `auto` tries
DuckDuckGo, then Bing. Reading also happens in the rendered DOM: an `article`,
`main`, or `[role=main]` container is stripped of navigation and scripts, then
re-flowed to the terminal width.

## Usage

```text
web-search [OPTIONS] <QUERY>...

Options:
  -n, --limit <N>       Number of results (default: 10, max: 50)
  -e, --engine <NAME>   Search engine: auto, bing, ddg, searx, brave
  -r, --read <N|URL>    Print result n, or the URL, as plain text
      --no-reader       Do not retry client-side pages through r.jina.ai
      --searx-url <URL> SearXNG base URL
      --backend <NAME>  Fetch backend: http or obscura [default: http]
      --obscura <PATH>  Obscura binary (default: obscura in PATH)
      --stealth         Pass --stealth to Obscura
      --json            Print results as JSON
      --urls            Print only the result URLs
      --color <WHEN>    Color output: auto, always, or never [default: auto]
  -t, --timeout <SECS>  Request timeout in seconds (default: 15)
  -v, --verbose         Print engine and timing to stderr
  -h, --help            Print help
  -V, --version         Print version
```

```bash
web-search rust async book
web-search -n 5 --urls "rust 2024 edition"
web-search --json "who invented unix" | jq -r '.[].url'
echo "tauri vs egui" | web-search -n 5
web-search -r 2 rust async book
web-search -r https://example.com/
web-search --backend obscura --stealth "site blocked plain clients"
```

## Notes

`auto` tries DuckDuckGo, then Brave when `BRAVE_API_KEY` is set, then SearXNG,
then Bing. The obscura backend supports DuckDuckGo and Bing. The last result
list is cached at `$XDG_CACHE_HOME/web-search/last.json`, which lets
`web-search -r 2` reopen a previous result.

## Config

Reads `~/.config/web-search/config.toml` if present. Flags override the file;
defaults apply otherwise.

## Install

**Requirements:** Rust 1.88+ and `git`. The `obscura` backend additionally needs
the Obscura binary on `PATH`.

```bash
cargo install --git https://github.com/batusaribay/web-search.git --locked
```

Obscura is optional. Install it from its
[releases](https://github.com/h4ckf0r0day/obscura/releases):

```bash
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-linux.tar.gz
tar xzf obscura-x86_64-linux.tar.gz
sudo install -m 755 obscura obscura-worker /usr/local/bin/
```

Keep `obscura` and `obscura-worker` together, because parallel scraping uses
both. macOS and Windows archives are published as well.

## License

This project is licensed under the [MIT License](LICENSE).
