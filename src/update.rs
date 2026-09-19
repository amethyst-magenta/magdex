use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/openai/codex/releases/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestVersion {
    pub version: String,
    pub dismissed: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct VersionCache {
    latest_version: String,
    dismissed_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexVersionCache {
    latest_version: String,
    dismissed_version: Option<String>,
}

pub async fn check_latest_version() -> Option<LatestVersion> {
    let path = cache_path();
    let cache = read_cache(&path);
    let codex_path = codex_cache_path();
    let official = codex_path.as_deref().and_then(read_codex_cache);

    let fresh_latest = newest_version(
        cache_is_fresh(&path)
            .then_some(cache.latest_version.as_str())
            .into_iter()
            .chain(
                codex_path
                    .as_deref()
                    .filter(|path| cache_is_fresh(path))
                    .and(official.as_ref())
                    .map(|cache| cache.latest_version.as_str()),
            ),
    );
    if let Some(latest_version) = fresh_latest {
        let next = merged_cache(latest_version, &cache, official.as_ref());
        let result = latest_from_cache(&next);
        let _ = write_cache(&path, &next);
        return Some(result);
    }

    let latest_version = match fetch_latest_version().await {
        Ok(version) => version,
        Err(_) => newest_version(
            Some(cache.latest_version.as_str())
                .into_iter()
                .chain(official.as_ref().map(|cache| cache.latest_version.as_str())),
        )?,
    };
    let next = merged_cache(latest_version, &cache, official.as_ref());
    let result = latest_from_cache(&next);
    let _ = write_cache(&path, &next);
    Some(result)
}

pub fn dismiss_version(version: &str) -> Result<()> {
    let path = cache_path();
    let mut cache = read_cache(&path);
    cache.latest_version = version.to_string();
    cache.dismissed_version = Some(version.to_string());
    write_cache(&path, &cache)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (release_version(latest), release_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

pub fn version_from_user_agent(user_agent: &str) -> Option<String> {
    user_agent
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .find_map(|part| release_version(part).map(|_| part.to_string()))
}

pub async fn run_update() -> Result<()> {
    let status = Command::new("codex")
        .arg("update")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await
        .context("cannot start `codex update`")?;
    ensure!(status.success(), "`codex update` failed with {status}");
    Ok(())
}

async fn fetch_latest_version() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let response = client
        .get(LATEST_RELEASE_URL)
        .header(reqwest::header::USER_AGENT, "magdex-update-check")
        .send()
        .await?
        .error_for_status()?;
    let body = response.json::<Value>().await?;
    let tag = body
        .get("tag_name")
        .and_then(Value::as_str)
        .context("latest Codex release has no tag_name")?;
    normalize_release_version(tag).context("latest Codex release tag is not a version")
}

fn read_codex_cache(path: &Path) -> Option<CodexVersionCache> {
    let cache = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<CodexVersionCache>(&text).ok())?;
    release_version(&cache.latest_version)?;
    Some(cache)
}

fn merged_cache(
    latest_version: String,
    cache: &VersionCache,
    official: Option<&CodexVersionCache>,
) -> VersionCache {
    let dismissed_version = [
        cache.dismissed_version.as_deref(),
        official.and_then(|cache| cache.dismissed_version.as_deref()),
    ]
    .into_iter()
    .flatten()
    .find(|dismissed| *dismissed == latest_version)
    .map(str::to_owned)
    .or_else(|| cache.dismissed_version.clone());
    VersionCache {
        latest_version,
        dismissed_version,
    }
}

fn newest_version<'a>(versions: impl Iterator<Item = &'a str>) -> Option<String> {
    versions
        .filter_map(|version| release_version(version).map(|parsed| (parsed, version)))
        .max_by_key(|(parsed, _)| *parsed)
        .map(|(_, version)| version.to_string())
}

fn latest_from_cache(cache: &VersionCache) -> LatestVersion {
    LatestVersion {
        version: cache.latest_version.clone(),
        dismissed: cache.dismissed_version.as_deref() == Some(&cache.latest_version),
    }
}

fn read_cache(path: &Path) -> VersionCache {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_cache(path: &Path, cache: &VersionCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec(cache)?)?;
    Ok(())
}

fn cache_is_fresh(path: &Path) -> bool {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age <= CHECK_INTERVAL)
}

fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("magdex/codex-version.json")
}

fn codex_cache_path() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .map(|home| home.join("version.json"))
}

fn normalize_release_version(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let version = trimmed
        .strip_prefix("rust-v")
        .or_else(|| trimmed.strip_prefix('v'))
        .unwrap_or(trimmed);
    release_version(version)?;
    Some(version.to_string())
}

fn release_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_release_versions_numerically() {
        assert!(is_newer("0.155.1", "0.154.0"));
        assert!(is_newer("1.0.0", "0.999.999"));
        assert!(!is_newer("0.154.0", "0.154.0"));
        assert!(!is_newer("not-a-version", "0.154.0"));
    }

    #[test]
    fn extracts_version_from_codex_user_agent() {
        assert_eq!(
            version_from_user_agent("codex_cli_rs/0.154.0 (Linux 6.12)"),
            Some("0.154.0".into())
        );
    }

    #[test]
    fn normalizes_codex_release_tags() {
        assert_eq!(
            normalize_release_version("rust-v0.155.1").as_deref(),
            Some("0.155.1")
        );
        assert_eq!(
            normalize_release_version("v0.155.1").as_deref(),
            Some("0.155.1")
        );
        assert_eq!(normalize_release_version("other").as_deref(), None);
    }

    #[test]
    fn newer_official_cache_wins_and_keeps_its_dismissal() {
        let local = VersionCache {
            latest_version: "0.154.0".into(),
            dismissed_version: Some("0.154.0".into()),
        };
        let official = CodexVersionCache {
            latest_version: "0.155.1".into(),
            dismissed_version: Some("0.155.1".into()),
        };
        let latest = newest_version(
            [
                local.latest_version.as_str(),
                official.latest_version.as_str(),
            ]
            .into_iter(),
        )
        .unwrap();
        let merged = merged_cache(latest, &local, Some(&official));

        assert_eq!(merged.latest_version, "0.155.1");
        assert_eq!(merged.dismissed_version.as_deref(), Some("0.155.1"));
    }
}
