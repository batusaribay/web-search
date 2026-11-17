# web-search

A Rust CLI for web search and reading.

![web-search in a terminal](preview.png)

## How it works

1. The query comes from the arguments or from piped stdin.
2. Each engine is fetched over TLS with `ureq` and `rustls`, a request timeout,
   and an 8 MiB response cap.
3. DuckDuckGo HTML, Bing RSS, SearXNG JSON, and the Brave API are normalized into
   the same result shape.
4. Titles and snippets are cleaned and wrapped to `COLUMNS` (80 when unset).
5. `--read` fetches the selected page once and extracts article text with
   `dom_smoothie`. If extraction is too small, it can retry through `r.jina.ai`
   unless `--no-reader` is set.

## Usage

### Commands

| Command | Action |
| --- | --- |
| `web-search <query...>` | Search and print ranked results. |
| `web-search -r <n\|url>` | Read result `n`, or a URL, as plain text. |
| `web-search --urls <query...>` | Print only result URLs. |
| `web-search --json <query...>` | Print JSON. |

### Examples

```bash
web-search rust async book
web-search -n 5 --urls "rust 2024 edition"
web-search --json "who invented unix" | jq -r '.[].url'
echo "tauri vs egui" | web-search -n 5
web-search -e brave -n 20 "embedded rust"
web-search -r 2 rust async book
web-search -r https://example.com/
```

### Options

| Option | Meaning |
| --- | --- |
| `-n, --limit <n>` | Number of results, 1–50 (default: 10). |
| `-e, --engine <name>` | `auto`, `bing`, `ddg`, `searx`, or `brave`. |
| `-r, --read <n\|url>` | Read a result number or URL as plain text. |
| `--no-reader` | Do not retry client-side pages through `r.jina.ai`. |
| `--searx-url <url>` | SearXNG base URL. |
| `--json` | Print a JSON array. |
| `--urls` | Print only result URLs. |
| `-t, --timeout <secs>` | Per-request timeout (default: 15). |
| `-v, --verbose` | Print engine and timing to stderr. |

Exit codes: `0` for a completed search or read, `1` for a failed request, `2`
for usage errors.

### Reading a result

```bash
web-search -r 2 rust async book
web-search -r 2                    # result 2 of the previous search
web-search -r https://example.com/
```

The last result list is cached at `$XDG_CACHE_HOME/web-search/last.json`.
Reading keeps only that list; queries are not logged.

### Engines

| Engine | Source |
| --- | --- |
| `ddg` | DuckDuckGo HTML, with the lite endpoint as a fallback. |
| `bing` | Bing RSS, pinned to the `en-US` market. |
| `searx` | SearXNG JSON; set `--searx-url` or `SEARX_URL`. |
| `brave` | Brave API; set `BRAVE_API_KEY`. |

`auto` tries DuckDuckGo, then Bing, then a SearXNG instance when `--searx-url`
or `SEARX_URL` is set. Bing results that share no word with the query are
dropped, so a zero-result page cannot turn into unrelated hits. Use `-v` to see
which engine answered and why the others were skipped.

### Limits

The HTML endpoints are not APIs. They can return bot checks or rate limits; use
the Brave API or a SearXNG instance for regular use. JavaScript-only pages still
read poorly, and PDFs are reported as unreadable.

If DuckDuckGo always times out on a VPN or Cloudflare WARP, run
`warp-cli tunnel host add duckduckgo.com` to route it outside the tunnel, or
use `--engine bing`.

DuckDuckGo can also bot-check an IP after repeated searches. The check is
temporary; `auto` reports it and falls back to Bing. After a bot check or a
failed connection, DuckDuckGo is skipped for 15 minutes so retries do not
extend the block. To retry sooner, remove `ddg-blocked` from the cache
directory (`~/.cache/web-search/` by default).

## Install

**Requirements:** Rust 1.88+ and `git`.

```bash
cargo install --git https://github.com/batusaribay/web-search.git --locked
```

## Uninstall

```bash
cargo uninstall web-search
```

## Security

- TLS is handled by `rustls`.
- Only the selected engine and result URL are contacted.
- `--no-reader` disables the optional `r.jina.ai` fallback.
- Response sizes are capped and request timeouts are bounded.
- No credentials, cookies, configuration, or telemetry are read or sent.
- Unsafe code is forbidden by the package lint configuration.

## License

This project is licensed under the [MIT License](LICENSE).
