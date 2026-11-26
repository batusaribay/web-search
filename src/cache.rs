use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::engines::Hit;

/// DuckDuckGo bot-checks are IP-based and temporary. Its block is a sliding
/// window that needs roughly an hour without requests to clear, so retrying
/// sooner only extends it.
const DDG_COOLDOWN: Duration = Duration::from_secs(65 * 60);

/// A real visitor does not send two searches in the same second. Stop scripts
/// and repeated runs from turning into the burst DDG looks for.
const DDG_MIN_GAP: Duration = Duration::from_secs(3);

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

    #[cfg(windows)]
    if let Some(directory) = env::var_os("LOCALAPPDATA")
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

    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if write_private(&temporary, &text).is_err() {
        let _ = fs::remove_file(&temporary);
        return None;
    }
    #[cfg(windows)]
    if path.exists() {
        let _ = fs::remove_file(path).ok()?;
    }
    if fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
        return None;
    }
    Some(())
}

/// The last query can be private, so keep it readable only by its owner.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }

    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(text.as_bytes())?;
    file.flush()
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

pub fn mark_ddg_blocked() {
    write_timestamp("ddg-blocked");
}

pub fn ddg_cooldown_remaining() -> Option<Duration> {
    let blocked_at = read_timestamp("ddg-blocked")?;
    cooldown_remaining_at(unix_now(), blocked_at)
}

pub fn mark_ddg_request() {
    write_timestamp("ddg-last");
}

pub fn ddg_request_wait() -> Option<Duration> {
    let last = read_timestamp("ddg-last")?;
    request_wait_at(unix_now(), last)
}

fn state_path(name: &str) -> Option<PathBuf> {
    Some(path()?.with_file_name(name))
}

fn write_timestamp(name: &str) {
    let Some(path) = state_path(name) else {
        return;
    };
    let _ = write_private(&path, &unix_now().to_string());
}

fn read_timestamp(name: &str) -> Option<u64> {
    fs::read_to_string(state_path(name)?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn cooldown_remaining_at(now: u64, blocked_at: u64) -> Option<Duration> {
    let elapsed = now.saturating_sub(blocked_at);
    let cooldown = DDG_COOLDOWN.as_secs();
    (elapsed < cooldown).then(|| Duration::from_secs(cooldown - elapsed))
}

fn request_wait_at(now: u64, last: u64) -> Option<Duration> {
    let elapsed = now.saturating_sub(last);
    let gap = DDG_MIN_GAP.as_secs();
    (elapsed < gap).then(|| Duration::from_secs(gap - elapsed))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
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
        std::env::temp_dir()
            .join(format!("web-search-cache-{}-{unique}", std::process::id()))
            .join("last.json")
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_missing_cache_is_not_an_error() {
        assert!(load_from(Path::new("/nonexistent/web-search/last.json")).is_none());
    }

    #[test]
    fn ddg_cooldown_expires() {
        assert_eq!(
            cooldown_remaining_at(1_000, 1_000),
            Some(Duration::from_secs(3_900))
        );
        assert_eq!(
            cooldown_remaining_at(1_000 + 3_300, 1_000),
            Some(Duration::from_secs(600))
        );
        assert_eq!(cooldown_remaining_at(1_000 + 3_900, 1_000), None);
        assert_eq!(cooldown_remaining_at(9_000, 1_000), None);
    }

    #[test]
    fn ddg_requests_are_paced() {
        assert_eq!(request_wait_at(1_000, 1_000), Some(Duration::from_secs(3)));
        assert_eq!(request_wait_at(1_002, 1_000), Some(Duration::from_secs(1)));
        assert_eq!(request_wait_at(1_003, 1_000), None);
        assert_eq!(request_wait_at(5_000, 1_000), None);
    }
}
