use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::engines::Hit;

/// The last result list, so a result number can be read after the search that
/// produced it. Without it, `web-search -r 2` would need the query again and
/// could land on a different ranking.
pub struct Entry {
    pub title: String,
    pub url: String,
}

pub struct Saved {
    pub results: Vec<Entry>,
}

pub fn save(query: &str, hits: &[Hit]) {
    if hits.is_empty() {
        return;
    }
    let Some(path) = path() else {
        return;
    };
    let _ = save_to(&path, query, hits);
}

pub fn load() -> Option<Saved> {
    load_from(&path()?)
}

pub fn path() -> Option<PathBuf> {
    if let Some(directory) = env::var_os("XDG_CACHE_HOME")
        && !directory.is_empty()
    {
        return Some(PathBuf::from(directory).join("web-search/last.json"));
    }

    let home = env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".cache/web-search/last.json"))
}

fn save_to(path: &Path, query: &str, hits: &[Hit]) -> Option<()> {
    let results: Vec<Value> = hits
        .iter()
        .map(|hit| {
            json!({
                "title": hit.title,
                "url": hit.url,
            })
        })
        .collect();
    let data = json!({
        "query": query,
        "results": results,
    });
    let text = serde_json::to_string_pretty(&data).ok()?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok()?;
    }

    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, text).ok()?;
    fs::rename(&temporary, path).ok()?;
    Some(())
}

fn load_from(path: &Path) -> Option<Saved> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let results = value.get("results")?.as_array()?;

    let results = results
        .iter()
        .filter_map(|result| {
            Some(Entry {
                title: result.get("title")?.as_str()?.to_string(),
                url: result.get("url")?.as_str()?.to_string(),
            })
        })
        .collect();

    Some(Saved { results })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn hit(title: &str, url: &str) -> Hit {
        Hit {
            title: title.to_string(),
            url: url.to_string(),
            snippet: String::new(),
            engine: "ddg",
        }
    }

    fn temporary_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "web-search-cache-{}-{unique}.json",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_the_last_results() {
        let path = temporary_path();
        save_to(
            &path,
            "rust lifetimes",
            &[
                hit("Rust", "https://www.rust-lang.org/"),
                hit("Book", "https://doc.rust-lang.org/book/"),
            ],
        )
        .unwrap();

        let saved = load_from(&path).unwrap();
        assert_eq!(saved.results.len(), 2);
        assert_eq!(saved.results[0].title, "Rust");
        assert_eq!(saved.results[1].url, "https://doc.rust-lang.org/book/");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_missing_cache_is_not_an_error() {
        assert!(load_from(Path::new("/nonexistent/web-search/last.json")).is_none());
    }
}
