//! Read-only view of Horizon's official game releases.
//!
//! The upstream repository does not currently publish Android binaries. This feed therefore
//! reports official build identities and exposes an Android download only when an ABI-matched
//! `.hruntime.zip` asset is actually present. Desktop archives are never treated as Android builds.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const API_URL: &str = "https://api.github.com/repos/horizon-rust/horizon/releases?per_page=100";
const MIRROR_URL: &str = "https://mcp.playhorizon.dev/github/public/repos/horizon-rust/horizon/releases?per_page=100";
const CACHE_TTL: u64 = 30 * 60;
const MAX_RELEASES: usize = 16;
const MAX_RESPONSE: u64 = 6 * 1024 * 1024;
const BLOCKED_COMMIT: &str = "c9d474d";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HorizonRelease {
    version: String,
    tag: String,
    name: String,
    date: String,
    commit: Option<String>,
    url: String,
    android_runtime_asset: Option<String>,
    android_runtime_url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Snapshot {
    abi: String,
    checked_at: u64,
    releases: Vec<HorizonRelease>,
    error: Option<String>,
}

#[derive(Default)]
struct State {
    loaded: bool,
    checking: bool,
    force_refresh: bool,
    last_attempt: u64,
    snapshot: Snapshot,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn cache_path(home: &Path) -> PathBuf {
    home.join("cache").join("official-horizon-game-releases.json")
}

fn load_cache(home: &Path, abi: &str) -> Snapshot {
    let path = cache_path(home);
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return Snapshot::default();
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 * 1024 {
        return Snapshot::default();
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Snapshot>(&bytes).ok())
        .filter(|snapshot| snapshot.abi == abi)
        .map(|mut snapshot| {
            snapshot.releases.retain(|release| {
                release.commit.as_deref() != Some(BLOCKED_COMMIT)
                    && release.version.rsplit('-').next() != Some(BLOCKED_COMMIT)
            });
            snapshot
        })
        .unwrap_or_default()
}

fn save_cache(home: &Path, snapshot: &Snapshot) {
    let path = cache_path(home);
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if std::fs::symlink_metadata(parent)
        .is_ok_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(snapshot) else { return };
    let temporary = path.with_extension("partial");
    if std::fs::write(&temporary, bytes).is_ok() {
        let _ = std::fs::rename(temporary, path);
    }
}

fn read_release_json(url: &str) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .user_agent("Horizon-Android-Plus/1.0")
        .timeout_connect(Duration::from_secs(12))
        .timeout_read(Duration::from_secs(20))
        .build();
    let response = agent
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(code, _) => format!("Release server answered HTTP {code}"),
            ureq::Error::Transport(error) => format!("Network error: {error}"),
        })?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err("Release list response exceeded its size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid release list: {error}"))
}

fn date_from_name(name: &str) -> Option<String> {
    name.as_bytes().windows(10).find_map(|candidate| {
        let valid = candidate[4] == b'-'
            && candidate[7] == b'-'
            && candidate
                .iter()
                .enumerate()
                .all(|(i, byte)| matches!(i, 4 | 7) || byte.is_ascii_digit());
        valid.then(|| std::str::from_utf8(candidate).ok().map(str::to_owned)).flatten()
    })
}

fn commit_from_name(name: &str) -> Option<String> {
    let candidate = name
        .rsplit('·')
        .next()?
        .trim()
        .split_whitespace()
        .next()?;
    (7..=40).contains(&candidate.len())
        .then_some(candidate)
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

fn abi_aliases(abi: &str) -> &'static [&'static str] {
    match abi {
        "arm64-v8a" => &["arm64-v8a", "aarch64-linux-android"],
        "armeabi-v7a" => &["armeabi-v7a", "armv7-linux-androideabi"],
        "x86" => &["x86", "i686-linux-android"],
        "x86_64" => &["x86_64", "x86-64", "x86_64-linux-android"],
        _ => &[],
    }
}

fn android_asset(release: &Value, abi: &str) -> Option<(String, String)> {
    let aliases = abi_aliases(abi);
    release.get("assets")?.as_array()?.iter().find_map(|asset| {
        let name = asset.get("name")?.as_str()?;
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".hruntime.zip")
            || !aliases.iter().any(|alias| lower.contains(alias))
        {
            return None;
        }
        let url = asset.get("browser_download_url")?.as_str()?;
        url.starts_with("https://")
            .then(|| (name.to_owned(), url.to_owned()))
    })
}

fn is_nightly_tag(tag: &str) -> bool {
    if tag == "nightly" {
        return true;
    }
    let Some(date) = tag.strip_prefix("nightly-") else {
        return false;
    };
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7) || byte.is_ascii_digit()
        })
}

fn is_horizon_game_tag(tag: &str) -> bool {
    is_nightly_tag(tag)
        || tag.strip_prefix('v').is_some_and(|value| {
            value.as_bytes().first().is_some_and(u8::is_ascii_digit)
        })
}

fn compare_releases(left: &HorizonRelease, right: &HorizonRelease) -> std::cmp::Ordering {
    match (left.tag == "nightly", right.tag == "nightly") {
        (true, false) => return std::cmp::Ordering::Less,
        (false, true) => return std::cmp::Ordering::Greater,
        _ => {}
    }
    match (is_nightly_tag(&left.tag), is_nightly_tag(&right.tag)) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (true, true) => right.date.cmp(&left.date),
        (false, false) => std::cmp::Ordering::Equal,
    }
}

fn parse_release(release: &Value, abi: &str) -> Option<HorizonRelease> {
    if release.get("draft").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let tag = release.get("tag_name")?.as_str()?.to_owned();
    if !is_horizon_game_tag(&tag) {
        return None;
    }
    let name = release
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&tag)
        .to_owned();
    let published = release
        .get("published_at")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let date = date_from_name(&name)
        .or_else(|| published.get(..10).map(str::to_owned))
        .unwrap_or_default();
    let commit = commit_from_name(&name);
    if commit.as_deref() == Some(BLOCKED_COMMIT) {
        return None;
    }
    let version = if is_nightly_tag(&tag) {
        match (date.is_empty(), commit.as_deref()) {
            (false, Some(commit)) => format!("nightly-{date}-{commit}"),
            (false, None) if tag == "nightly" => format!("nightly-{date}"),
            (true, _) if tag == "nightly" => "nightly".to_owned(),
            _ => tag.clone(),
        }
    } else {
        tag.clone()
    };
    let url = release
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://github.com/"))
        .unwrap_or("https://github.com/horizon-rust/horizon/releases")
        .to_owned();
    let asset = android_asset(release, abi);
    Some(HorizonRelease {
        version,
        tag,
        name,
        date,
        commit,
        url,
        android_runtime_asset: asset.as_ref().map(|(name, _)| name.clone()),
        android_runtime_url: asset.map(|(_, url)| url),
    })
}

fn fetch_snapshot(abi: &str) -> Result<Snapshot, String> {
    let value = match read_release_json(API_URL) {
        Ok(value) => value,
        Err(primary) => read_release_json(MIRROR_URL)
            .map_err(|mirror| format!("{primary}; Horizon's release mirror also failed: {mirror}"))?,
    };
    let releases = value
        .as_array()
        .ok_or_else(|| "Release list did not contain an array".to_owned())?;
    let mut horizon_releases = releases
        .iter()
        .filter_map(|release| parse_release(release, abi))
        .collect::<Vec<_>>();
    horizon_releases.sort_by(compare_releases);
    horizon_releases.truncate(MAX_RELEASES);
    if horizon_releases.is_empty() {
        return Err("Horizon's release feed did not contain any game builds".into());
    }
    Ok(Snapshot {
        abi: abi.to_owned(),
        checked_at: now(),
        releases: horizon_releases,
        error: None,
    })
}

/// Requests an immediate background refresh on the next status poll.
pub fn refresh() {
    let mut guard = state().lock().unwrap_or_else(|error| error.into_inner());
    if !guard.checking {
        guard.force_refresh = true;
    }
}

/// Starts a background refresh when the cached feed is older than 30 minutes, and returns the
/// last known official releases immediately so status polling never blocks the game thread.
pub fn status(home: &Path, abi: &str) -> Value {
    let mut guard = state().lock().unwrap_or_else(|error| error.into_inner());
    if !guard.loaded {
        guard.snapshot = load_cache(home, abi);
        guard.last_attempt = guard.snapshot.checked_at;
        guard.loaded = true;
    }
    let current_time = now();
    let stale = guard.force_refresh
        || guard.snapshot.checked_at == 0
        || current_time.saturating_sub(guard.snapshot.checked_at) >= CACHE_TTL;
    let retry_ready = current_time.saturating_sub(guard.last_attempt) >= CACHE_TTL;
    if stale && (retry_ready || guard.force_refresh) && !guard.checking {
        guard.checking = true;
        guard.force_refresh = false;
        guard.last_attempt = current_time;
        let home = home.to_owned();
        let abi = abi.to_owned();
        let spawned = std::thread::Builder::new()
            .name("Horizon game release feed".into())
            .spawn(move || {
                let result = fetch_snapshot(&abi);
                let mut guard = state().lock().unwrap_or_else(|error| error.into_inner());
                match result {
                    Ok(snapshot) => {
                        save_cache(&home, &snapshot);
                        guard.snapshot = snapshot;
                    }
                    Err(error) => {
                        guard.snapshot.error = Some(error);
                    }
                }
                guard.checking = false;
            });
        if spawned.is_err() {
            guard.checking = false;
            guard.snapshot.error = Some("Could not start the Horizon release feed worker".into());
        }
    }
    let latest = guard.snapshot.releases.first().cloned();
    // Before the first downloaded Nightly, the APK's bundled runtime is the active
    // follow-latest baseline. This avoids downloading the same commit again just
    // because horizon-runtime-follow.txt has not been created yet.
    let followed = std::fs::read_to_string(home.join("horizon-runtime-follow.txt"))
        .or_else(|_| std::fs::read_to_string(home.join("horizon-runtime-default.txt")))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| crate::catalog::valid_runtime_id(value));
    let followed_commit = followed.as_ref().and_then(|id| {
        let manifest_path = home.join("horizon-runtimes").join(id).join("runtime.json");
        let metadata = std::fs::symlink_metadata(&manifest_path).ok()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 {
            return None;
        }
        let manifest: Value = serde_json::from_slice(&std::fs::read(manifest_path).ok()?).ok()?;
        manifest.get("commit")?.as_str().map(str::to_owned)
    });
    serde_json::json!({
        "checking": guard.checking,
        "checkedAt": guard.snapshot.checked_at,
        "error": guard.snapshot.error,
        "latest": latest,
        "recent": guard.snapshot.releases,
        "abi": abi,
        "followedRuntimeVersion": followed,
        "followedRuntimeCommit": followed_commit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nightly_title_becomes_the_official_install_version() {
        let release = serde_json::json!({
            "tag_name": "nightly",
            "name": "Nightly 2026-11-01 · 4aa6854",
            "published_at": "2026-11-01T15:39:00Z",
            "html_url": "https://github.com/horizon-rust/horizon/releases/tag/nightly",
            "assets": []
        });
        let parsed = parse_release(&release, "arm64-v8a").unwrap();
        assert_eq!(parsed.version, "nightly-2026-11-01-4aa6854");
        assert_eq!(parsed.commit.as_deref(), Some("4aa6854"));
        assert!(parsed.android_runtime_url.is_none());
    }

    #[test]
    fn blocked_nightly_commit_is_not_offered() {
        let release = serde_json::json!({
            "tag_name": "nightly",
            "name": format!("Nightly 2026-11-01 · {BLOCKED_COMMIT}"),
            "published_at": "2026-11-01T19:09:00Z",
            "assets": []
        });
        assert!(parse_release(&release, "arm64-v8a").is_none());
    }

    #[test]
    fn current_rolling_nightly_precedes_a_historical_tag_from_the_same_day() {
        let rolling = serde_json::json!({
            "tag_name": "nightly",
            "name": "Nightly 2026-11-01 · 7f3a2b1",
            "published_at": "2026-11-01T19:09:00Z",
            "assets": []
        });
        let dated = serde_json::json!({
            "tag_name": "nightly-2026-11-01",
            "name": "Nightly 2026-11-01 · 4aa6854",
            "published_at": "2026-11-01T15:39:00Z",
            "assets": []
        });
        let mut releases = vec![
            parse_release(&dated, "arm64-v8a").unwrap(),
            parse_release(&rolling, "arm64-v8a").unwrap(),
        ];
        releases.sort_by(|left, right| {
            (right.tag == "nightly")
                .cmp(&(left.tag == "nightly"))
                .then_with(|| right.date.cmp(&left.date))
        });
        assert_eq!(releases[0].tag, "nightly");
        assert_eq!(releases[0].version, "nightly-2026-11-01-7f3a2b1");
        assert_eq!(releases[1].version, "nightly-2026-11-01-4aa6854");
    }

    #[test]
    fn official_feed_includes_horizon_game_releases_but_not_launcher_releases() {
        let old_release = serde_json::json!({
            "tag_name": "v0.2.95",
            "name": "Horizon 0.2.95",
            "published_at": "2026-10-27T12:00:00Z",
            "html_url": "https://github.com/horizon-rust/horizon/releases/tag/v0.2.95",
            "assets": []
        });
        let dated_release = serde_json::json!({
            "tag_name": "v2026.10.19.15",
            "name": "Horizon 2026.10.19.15",
            "published_at": "2026-10-19T12:00:00Z",
            "assets": []
        });
        let launcher_release = serde_json::json!({
            "tag_name": "launcher@0.2.36",
            "name": "Horizon launcher 0.2.36",
            "published_at": "2026-11-02T03:12:00Z",
            "assets": []
        });
        assert_eq!(parse_release(&old_release, "arm64-v8a").unwrap().version, "v0.2.95");
        assert_eq!(parse_release(&dated_release, "arm64-v8a").unwrap().version, "v2026.10.19.15");
        assert!(parse_release(&launcher_release, "arm64-v8a").is_none());
    }

    #[test]
    fn release_order_keeps_current_nightly_then_previous_nightlies_then_old_game_releases() {
        let current = serde_json::json!({
            "tag_name": "nightly",
            "name": "Nightly 2026-11-01 · 7f3a2b1",
            "published_at": "2026-11-01T19:09:00Z",
            "assets": []
        });
        let previous = serde_json::json!({
            "tag_name": "nightly-2026-10-31",
            "name": "Nightly 2026-10-31 · d91cab6",
            "published_at": "2026-10-31T16:58:00Z",
            "assets": []
        });
        let old = serde_json::json!({
            "tag_name": "v0.2.95",
            "name": "Horizon 0.2.95",
            "published_at": "2026-10-27T12:00:00Z",
            "assets": []
        });
        let mut releases = [current, previous, old]
            .iter()
            .filter_map(|release| parse_release(release, "arm64-v8a"))
            .collect::<Vec<_>>();
        releases.sort_by(compare_releases);
        assert_eq!(releases.iter().map(|release| release.tag.as_str()).collect::<Vec<_>>(),
            vec!["nightly", "nightly-2026-10-31", "v0.2.95"]);
    }

    #[test]
    fn desktop_archives_are_never_mistaken_for_android_runtimes() {
        let release = serde_json::json!({
            "tag_name": "nightly",
            "name": "Nightly 2026-11-01 · 4aa6854",
            "published_at": "2026-11-01T15:39:00Z",
            "assets": [{
                "name": "horizon-linux-x86_64.tar.gz",
                "browser_download_url": "https://github.com/horizon-rust/horizon/releases/download/nightly/horizon-linux-x86_64.tar.gz"
            }]
        });
        let parsed = parse_release(&release, "x86_64").unwrap();
        assert!(parsed.android_runtime_url.is_none());
    }
}
