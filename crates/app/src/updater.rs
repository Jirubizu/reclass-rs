//! Self-update: ask the GitHub Releases API for the newest tag, compare it with
//! the compiled-in version and, on request, download and swap in the published
//! Linux tarball.
//!
//! HTTP goes through `curl` instead of a Rust client. The app already ships as a
//! Linux-only binary where `curl` is universally present, so shelling out avoids
//! pulling a TLS stack — and its certificate-store configuration — into a build
//! that otherwise has no network dependency at all.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow, bail};

/// `owner/repo` the releases are published under.
const REPO: &str = "Jirubizu/reclass-rs";
/// Release asset produced by the `release-build` CI job.
const ASSET: &str = "reclass-linux-x86_64.tar.gz";

/// Semantic version triple; any pre-release/build suffix is discarded.
pub type Version = (u32, u32, u32);

/// Render a [`Version`] the way tags are written.
pub fn format_version((major, minor, patch): Version) -> String {
    format!("v{major}.{minor}.{patch}")
}

/// The version this binary was compiled as.
pub fn current() -> Version {
    parse_version(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is a semver triple")
}

/// Parse `v1.2.3`, `1.2.3-beta` or `1.2` into a comparable triple.
///
/// Missing components default to `0`; a pre-release (`-`) or build (`+`) suffix
/// is ignored, so `v0.1.0-beta` and `v0.1.0` compare equal.
pub fn parse_version(s: &str) -> Option<Version> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// A published release worth offering to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Git tag, as published (e.g. `v0.4.0`).
    pub tag: String,
    /// [`tag`](Self::tag) parsed for comparison.
    pub version: Version,
    /// Release body — the generated changelog.
    pub notes: String,
    /// Direct download URL of the release tarball, when the release ships
    /// one. Older releases predate the packaged build and can only be
    /// installed by hand.
    pub asset_url: Option<String>,
}

/// What the update UI should currently show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing requested yet.
    Idle,
    /// A check is in flight.
    Checking,
    /// The newest release is not newer than [`current`].
    UpToDate,
    /// A newer release exists.
    Available(Release),
    /// A download/swap is in flight.
    Installing,
    /// Installed; the new binary takes effect on restart.
    Installed(Version),
    /// The last operation failed, with a display-ready reason.
    Failed(String),
}

/// Update checker driving background work for the GUI.
///
/// Checks and installs run on their own thread and publish into a shared
/// [`Status`] the UI polls each frame, so neither blocks rendering.
#[derive(Clone)]
pub struct Updater {
    status: Arc<Mutex<Status>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::Idle)),
        }
    }
}

/// Take the status lock, recovering from a poisoned mutex: a panicked worker
/// must not wedge the update UI for the rest of the session.
fn lock(slot: &Mutex<Status>) -> MutexGuard<'_, Status> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

impl Updater {
    /// Current status snapshot.
    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// Start a release check unless one is already running.
    pub fn check(&self) {
        {
            let mut st = lock(&self.status);
            if matches!(*st, Status::Checking | Status::Installing) {
                return;
            }
            *st = Status::Checking;
        }
        let slot = Arc::clone(&self.status);
        std::thread::spawn(move || {
            let next = match fetch_latest() {
                Ok(rel) if rel.version > current() => Status::Available(rel),
                Ok(_) => Status::UpToDate,
                Err(e) => Status::Failed(format!("{e:#}")),
            };
            *lock(&slot) = next;
        });
    }

    /// Download and install the release currently offered by [`Status::Available`].
    /// Does nothing in any other state.
    pub fn install(&self) {
        let rel = {
            let st = lock(&self.status);
            match &*st {
                Status::Available(rel) => rel.clone(),
                _ => return,
            }
        };
        *lock(&self.status) = Status::Installing;
        let slot = Arc::clone(&self.status);
        std::thread::spawn(move || {
            let next = match install(&rel) {
                Ok(()) => Status::Installed(rel.version),
                Err(e) => Status::Failed(format!("{e:#}")),
            };
            *lock(&slot) = next;
        });
    }

    /// Return to [`Status::Idle`], e.g. when the user dismisses an error.
    pub fn reset(&self) {
        *lock(&self.status) = Status::Idle;
    }
}

/// Run `curl` with the shared hardening flags and return its stdout.
fn curl(args: &[&str]) -> Result<String> {
    let out = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--max-time",
            "300",
            "--user-agent",
            concat!("reclass-rs/", env!("CARGO_PKG_VERSION")),
        ])
        .args(args)
        .output()
        .context("running `curl` (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("curl returned non-UTF-8 output")
}

/// Fetch the newest published release from the GitHub API.
pub fn fetch_latest() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = curl(&["-H", "Accept: application/vnd.github+json", &url])
        .context("querying the GitHub releases API")?;
    parse_release(&body)
}

/// Extract the fields we need from a GitHub `releases/latest` payload.
pub fn parse_release(json: &str) -> Result<Release> {
    let v: serde_json::Value =
        serde_json::from_str(json).context("parsing the GitHub release JSON")?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow!("release JSON has no `tag_name`"))?
        .to_string();
    let version =
        parse_version(&tag).ok_or_else(|| anyhow!("release tag `{tag}` is not a version"))?;
    // A missing asset is not a parse failure: releases published before the
    // packaged build exist, and the user still deserves an "up to date" answer
    // rather than an error. Installing is what actually needs the tarball.
    let asset_url = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str() == Some(ASSET))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(str::to_string);
    Ok(Release {
        tag,
        version,
        notes: v["body"].as_str().unwrap_or_default().trim().to_string(),
        asset_url,
    })
}

/// Download `rel`, then replace the running binary and its adjacent plugin
/// bundle. The swap takes effect on the next launch.
pub fn install(rel: &Release) -> Result<()> {
    let asset_url = rel
        .asset_url
        .as_deref()
        .ok_or_else(|| anyhow!("release {} publishes no `{ASSET}`", rel.tag))?;
    let exe = std::env::current_exe().context("locating the running binary")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("`{}` has no parent directory", exe.display()))?;

    let tmp = std::env::temp_dir().join(format!("reclass-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).with_context(|| format!("creating `{}`", tmp.display()))?;

    let pkg = tmp.join(ASSET);
    curl(&["--output", &pkg.to_string_lossy(), asset_url])
        .with_context(|| format!("downloading {asset_url}"))?;

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&pkg)
        .arg("-C")
        .arg(&tmp)
        .status()
        .context("running `tar`")?;
    if !status.success() {
        bail!("tar failed to extract {ASSET} ({status})");
    }

    replace(&tmp.join("reclass"), &exe)?;
    // The tarball ships the default plugins next to the binary; the loader
    // rejects a bundle built by another toolchain, so they move as a pair.
    let src_plugins = tmp.join("plugins");
    if src_plugins.is_dir() {
        let dst = dir.join("plugins");
        std::fs::create_dir_all(&dst).with_context(|| format!("creating `{}`", dst.display()))?;
        for entry in std::fs::read_dir(&src_plugins)?.flatten() {
            replace(&entry.path(), &dst.join(entry.file_name()))?;
        }
    }

    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Put `src` at `dst`, replacing whatever is there.
///
/// Staging inside `dst`'s directory keeps the final step a same-filesystem
/// `rename`, which swaps the directory entry atomically. The running process
/// keeps its already-mapped inode, so replacing a live binary or a loaded `.so`
/// is safe — writing over one in place would fail with `ETXTBSY`.
fn replace(src: &Path, dst: &Path) -> Result<()> {
    let dir = dst
        .parent()
        .ok_or_else(|| anyhow!("`{}` has no parent directory", dst.display()))?;
    let name = dst
        .file_name()
        .ok_or_else(|| anyhow!("`{}` has no file name", dst.display()))?;
    let staged = dir.join(format!(".{}.new", name.to_string_lossy()));

    std::fs::copy(src, &staged).with_context(|| {
        format!(
            "staging into `{}` (is it writable? reclass may need reinstalling by hand)",
            dir.display()
        )
    })?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("setting permissions on `{}`", staged.display()))?;
    std::fs::rename(&staged, dst).with_context(|| format!("replacing `{}`", dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tag_shapes() {
        assert_eq!(parse_version("v0.3.1"), Some((0, 3, 1)));
        assert_eq!(parse_version("0.3.1"), Some((0, 3, 1)));
        assert_eq!(parse_version("v0.1.0-beta"), Some((0, 1, 0)));
        assert_eq!(parse_version("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert_eq!(parse_version("nightly"), None);
    }

    #[test]
    fn version_order_drives_the_offer() {
        assert!(parse_version("v0.4.0") > parse_version("v0.3.1"));
        assert!(parse_version("v0.3.2") > parse_version("v0.3.1"));
        assert!(parse_version("v0.3.1") == parse_version("v0.3.1"));
        // a pre-release of the version we run is not an upgrade
        assert!(parse_version("v0.3.1-rc1") <= parse_version("v0.3.1"));
    }

    #[test]
    fn current_version_is_the_crate_version() {
        assert_eq!(current(), parse_version(env!("CARGO_PKG_VERSION")).unwrap());
    }

    // Three hashes: the body itself contains `"##`, which would close `r##"`.
    const SAMPLE: &str = r###"{
        "tag_name": "v0.9.2",
        "body": "## Changes since v0.9.1\n\n* fix: a thing\n",
        "assets": [
            {"name": "reclass-linux-x86_64.tar.gz.sha256",
             "browser_download_url": "https://example.invalid/sha"},
            {"name": "reclass-linux-x86_64.tar.gz",
             "browser_download_url": "https://example.invalid/tar"}
        ]
    }"###;

    #[test]
    fn picks_the_tarball_asset() {
        let rel = parse_release(SAMPLE).unwrap();
        assert_eq!(rel.tag, "v0.9.2");
        assert_eq!(rel.version, (0, 9, 2));
        assert_eq!(
            rel.asset_url.as_deref(),
            Some("https://example.invalid/tar")
        );
        assert!(rel.notes.starts_with("## Changes since v0.9.1"));
    }

    // An asset-less release must still yield a version, so an older published
    // release can answer "up to date" instead of erroring out.
    #[test]
    fn a_release_without_our_asset_still_parses_but_cannot_install() {
        let json = r#"{"tag_name": "v1.0.0", "assets": []}"#;
        let rel = parse_release(json).unwrap();
        assert_eq!(rel.version, (1, 0, 0));
        assert_eq!(rel.asset_url, None);
        let err = install(&rel).unwrap_err().to_string();
        assert!(err.contains("publishes no"), "{err}");
    }

    #[test]
    fn rejects_an_unparsable_tag() {
        let json = r#"{"tag_name": "nightly", "assets": []}"#;
        assert!(parse_release(json).is_err());
    }

    #[test]
    fn install_is_inert_until_a_release_is_offered() {
        let up = Updater::default();
        assert_eq!(up.status(), Status::Idle);
        up.install();
        assert_eq!(up.status(), Status::Idle);
    }

    // `replace` is the load-bearing half of the install: it must overwrite an
    // existing file, leave it executable, and drop its staging file.
    #[test]
    fn replace_overwrites_and_marks_executable() {
        let dir = std::env::temp_dir().join(format!("reclass-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src");
        let dst = dir.join("reclass");
        std::fs::write(&src, b"new").unwrap();
        std::fs::write(&dst, b"old").unwrap();
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o644)).unwrap();

        replace(&src, &dst).unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "{mode:o}");
        assert!(
            !dir.join(".reclass.new").exists(),
            "staging file left behind"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
