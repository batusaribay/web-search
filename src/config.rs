use std::env;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::engines::{Backend, Engine};
use crate::theme::ColorMode;

const MAX_LIMIT: usize = 50;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub limit: Option<usize>,
    pub engine: Option<String>,
    pub backend: Option<String>,
    pub searx_url: Option<String>,
    pub timeout: Option<u64>,
    pub color: Option<String>,
}

impl Config {
    pub fn engine(&self) -> Result<Option<Engine>, String> {
        self.engine
            .as_deref()
            .map(|name| {
                Engine::parse(name).ok_or_else(|| {
                    format!(
                        "unknown engine '{name}' in {}; use: {}",
                        config_path().display(),
                        Engine::NAMES
                    )
                })
            })
            .transpose()
    }

    pub fn backend(&self) -> Result<Option<Backend>, String> {
        self.backend
            .as_deref()
            .map(|name| {
                Backend::parse(name).ok_or_else(|| {
                    format!(
                        "unknown backend '{name}' in {}; use: {}",
                        config_path().display(),
                        Backend::NAMES
                    )
                })
            })
            .transpose()
    }

    pub fn color(&self) -> Result<Option<ColorMode>, String> {
        self.color
            .as_deref()
            .map(|name| {
                ColorMode::parse(name).ok_or_else(|| {
                    format!(
                        "unknown color mode '{name}' in {}; use: auto, always, never",
                        config_path().display()
                    )
                })
            })
            .transpose()
    }

    fn validate(&self) -> Result<(), String> {
        if self
            .limit
            .is_some_and(|limit| !(1..=MAX_LIMIT).contains(&limit))
        {
            return Err(format!(
                "limit in {} must be between 1 and {MAX_LIMIT}",
                config_path().display()
            ));
        }
        if self.timeout == Some(0) {
            return Err(format!(
                "timeout in {} must be at least 1 second",
                config_path().display()
            ));
        }
        if self
            .searx_url
            .as_deref()
            .is_some_and(|url| url.trim().is_empty())
        {
            return Err(format!(
                "searx_url in {} must not be empty",
                config_path().display()
            ));
        }
        self.engine()?;
        self.backend()?;
        self.color()?;
        Ok(())
    }
}

pub fn load() -> Result<Config, String> {
    let path = config_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    let config: Config = toml::from_str(&text)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    config.validate()?;
    Ok(config)
}

pub fn config_path() -> PathBuf {
    config_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("web-search")
        .join("config.toml")
}

fn config_home() -> Option<PathBuf> {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME")
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    #[cfg(windows)]
    if let Some(path) = env::var_os("APPDATA")
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_defaults() {
        let config: Config = toml::from_str(
            "limit = 5\nengine = \"bing\"\nbackend = \"http\"\ntimeout = 20\ncolor = \"never\"",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.limit, Some(5));
        assert_eq!(config.engine().unwrap(), Some(Engine::Bing));
        assert_eq!(config.backend().unwrap(), Some(Backend::Http));
        assert_eq!(config.color().unwrap(), Some(ColorMode::Never));
    }

    #[test]
    fn rejects_invalid_and_unknown_settings() {
        for source in [
            "limit = 0",
            "limit = 51",
            "timeout = 0",
            "searx_url = \" \"",
            "engine = \"example\"",
            "backend = \"example\"",
            "color = \"rainbow\"",
        ] {
            let config: Config = toml::from_str(source).unwrap();
            assert!(config.validate().is_err(), "{source}");
        }
        assert!(toml::from_str::<Config>("limt = 5").is_err());
    }

    #[test]
    fn locates_the_standard_config_path() {
        assert!(config_path().ends_with("web-search/config.toml"));
    }
}
