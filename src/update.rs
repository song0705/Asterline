//! Windows installer-managed update checks.
//!
//! Portable copies never rewrite themselves. An Inno Setup installation is
//! identified by a marker next to the executable. At most once per day, the
//! installed app checks the latest stable GitHub Release, verifies the setup
//! executable against that release's SHA256SUMS, and starts the installer with
//! a request to wait for this Asterline process to exit before replacing files.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::domain::event::RuntimeEvent;

const RELEASE_API: &str = "https://api.github.com/repos/song0705/Asterline/releases/latest";
const INSTALL_MARKER: &str = ".asterline-installer-managed";
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const STALE_LOCK_AGE: Duration = Duration::from_secs(10 * 60);
const MAX_INSTALLER_BYTES: u64 = 128 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum UpdateOutcome {
    Current,
    NotInstallerManaged,
    Scheduled(Version),
    AlreadyRunning,
    SkippedRecently,
}

impl fmt::Display for UpdateOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current => write!(formatter, "Asterline is up to date"),
            Self::NotInstallerManaged => write!(
                formatter,
                "automatic updates require the Windows Setup installation"
            ),
            Self::Scheduled(version) => write!(
                formatter,
                "Asterline {version} is ready and will install after Asterline exits"
            ),
            Self::AlreadyRunning => write!(formatter, "another update check is already running"),
            Self::SkippedRecently => write!(formatter, "update check skipped (checked recently)"),
        }
    }
}

struct UpdateLock {
    path: PathBuf,
    token: String,
}

impl UpdateLock {
    fn acquire(state_dir: &Path) -> Result<Option<Self>, String> {
        let path = state_dir.join("update.lock");
        let token = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file
                        .write_all(token.as_bytes())
                        .and_then(|()| file.sync_all())
                    {
                        fs::remove_file(&path).ok();
                        return Err(format!("could not initialize update lock: {error}"));
                    }
                    return Ok(Some(Self { path, token }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !lock_is_stale(&path, SystemTime::now()) {
                        return Ok(None);
                    }
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!("could not clear stale update lock: {error}"));
                        }
                    }
                }
                Err(error) => return Err(format!("could not acquire update lock: {error}")),
            }
        }
        Ok(None)
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let owns_lock = fs::read_to_string(&self.path)
            .map(|contents| contents == self.token.as_str())
            .unwrap_or(false);
        if owns_lock {
            fs::remove_file(&self.path).ok();
        }
    }
}

pub(crate) fn update_now() -> Result<UpdateOutcome, String> {
    check_and_schedule(true)
}

pub(crate) fn spawn_auto_update(notices: SyncSender<RuntimeEvent>) {
    if !is_installer_managed() {
        return;
    }

    std::thread::Builder::new()
        .name("asterline-updater".to_string())
        .spawn(move || {
            if let Ok(UpdateOutcome::Scheduled(version)) = check_and_schedule(false) {
                let _ = notices.try_send(RuntimeEvent::Notice(format!(
                    "Asterline {version} downloaded; it will install after exit"
                )));
            }
        })
        .ok();
}

fn check_and_schedule(force: bool) -> Result<UpdateOutcome, String> {
    if !is_installer_managed() {
        return Ok(UpdateOutcome::NotInstallerManaged);
    }

    let state_dir = update_state_dir()?;
    fs::create_dir_all(&state_dir)
        .map_err(|error| format!("could not create update directory: {error}"))?;
    let Some(_lock) = UpdateLock::acquire(&state_dir)? else {
        return Ok(UpdateOutcome::AlreadyRunning);
    };
    let checked_path = state_dir.join("last-check");
    if !force && !check_is_due(&checked_path, SystemTime::now()) {
        return Ok(UpdateOutcome::SkippedRecently);
    }

    let agent = update_agent();
    let release: Release =
        serde_json::from_str(&get_text(&agent, RELEASE_API, MAX_METADATA_BYTES)?)
            .map_err(|error| format!("invalid release metadata: {error}"))?;

    let latest = parse_release_version(&release.tag_name)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("invalid installed version: {error}"))?;
    if latest <= current {
        record_check(&checked_path, &release.tag_name)?;
        cleanup_cached_installers(&state_dir);
        return Ok(UpdateOutcome::Current);
    }

    let installer_name = installer_name(&latest);
    let installer_asset = find_asset(&release, &installer_name)?;
    let checksums_asset = find_asset(&release, "SHA256SUMS")?;
    let checksums = get_text(
        &agent,
        &checksums_asset.browser_download_url,
        MAX_METADATA_BYTES,
    )?;
    let expected = checksum_for(&checksums, &installer_name)?;
    let installer_path = state_dir.join(&installer_name);

    if installer_path.is_file() && sha256_file(&installer_path)? != expected {
        fs::remove_file(&installer_path)
            .map_err(|error| format!("could not replace stale installer: {error}"))?;
    }
    if !installer_path.is_file() {
        download_verified(
            &agent,
            &installer_asset.browser_download_url,
            &installer_path,
            &expected,
        )?;
    }

    schedule_installer(&installer_path)?;
    record_check(&checked_path, &release.tag_name)?;
    Ok(UpdateOutcome::Scheduled(latest))
}

fn record_check(path: &Path, tag: &str) -> Result<(), String> {
    fs::write(path, tag.as_bytes())
        .map_err(|error| format!("could not record update check: {error}"))
}

fn update_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .build(),
        )
        .build()
        .new_agent()
}

fn get_text(agent: &ureq::Agent, url: &str, limit: u64) -> Result<String, String> {
    agent
        .get(url)
        .header(
            "User-Agent",
            concat!("Asterline/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| format!("request failed: {error}"))?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()
        .map_err(|error| format!("could not read response: {error}"))
}

fn find_asset<'a>(release: &'a Release, name: &str) -> Result<&'a ReleaseAsset, String> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| format!("release asset not found: {name}"))
}

fn parse_release_version(tag: &str) -> Result<Version, String> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag))
        .map_err(|error| format!("invalid release tag {tag}: {error}"))
}

fn installer_name(version: &Version) -> String {
    format!("asterline-{version}-x86_64-windows-setup.exe")
}

fn checksum_for(contents: &str, filename: &str) -> Result<String, String> {
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(checksum) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        if name == filename
            && checksum.len() == 64
            && checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(checksum.to_ascii_lowercase());
        }
    }
    Err(format!("SHA256SUMS has no valid entry for {filename}"))
}

fn download_verified(
    agent: &ureq::Agent,
    url: &str,
    destination: &Path,
    expected: &str,
) -> Result<(), String> {
    let partial = destination.with_extension("exe.part");
    let result = (|| {
        let mut response = agent
            .get(url)
            .header(
                "User-Agent",
                concat!("Asterline/", env!("CARGO_PKG_VERSION")),
            )
            .call()
            .map_err(|error| format!("installer download failed: {error}"))?;
        let mut reader = response.body_mut().as_reader();
        let mut file = File::create(&partial)
            .map_err(|error| format!("could not create installer download: {error}"))?;
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];

        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| format!("could not read installer download: {error}"))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > MAX_INSTALLER_BYTES {
                return Err("installer download exceeded 128 MiB".to_string());
            }
            file.write_all(&buffer[..read])
                .map_err(|error| format!("could not save installer download: {error}"))?;
            hasher.update(&buffer[..read]);
        }
        file.sync_all()
            .map_err(|error| format!("could not sync installer download: {error}"))?;

        let actual = hex_digest(hasher.finalize().as_slice());
        if actual != expected {
            return Err(format!(
                "installer checksum mismatch: expected {expected}, got {actual}"
            ));
        }
        fs::rename(&partial, destination)
            .map_err(|error| format!("could not finalize installer download: {error}"))?;
        Ok(())
    })();

    if result.is_err() {
        fs::remove_file(&partial).ok();
    }
    result
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("could not open downloaded installer: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not read downloaded installer: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn schedule_installer(path: &Path) -> Result<(), String> {
    Command::new(path)
        .arg("/VERYSILENT")
        .arg("/SUPPRESSMSGBOXES")
        .arg("/NORESTART")
        .arg("/NORESTARTAPPLICATIONS")
        .arg("/SP-")
        .arg(format!("/WAITPID={}", std::process::id()))
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not start the update installer: {error}"))
}

fn is_installer_managed() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(INSTALL_MARKER)))
        .is_some_and(|marker| marker.is_file())
}

fn update_state_dir() -> Result<PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "LOCALAPPDATA is not set".to_string())?;
    Ok(PathBuf::from(local).join("Asterline").join("updates"))
}

fn check_is_due(stamp: &Path, now: SystemTime) -> bool {
    let Ok(modified) = stamp.metadata().and_then(|metadata| metadata.modified()) else {
        return true;
    };
    now.duration_since(modified)
        .map_or(true, |elapsed| elapsed >= CHECK_INTERVAL)
}

fn lock_is_stale(path: &Path, now: SystemTime) -> bool {
    let Ok(modified) = path.metadata().and_then(|metadata| metadata.modified()) else {
        return true;
    };
    now.duration_since(modified)
        .map_or(true, |elapsed| elapsed >= STALE_LOCK_AGE)
}

fn cleanup_cached_installers(state_dir: &Path) {
    let Ok(entries) = fs::read_dir(state_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("asterline-")
            && (name.ends_with("-x86_64-windows-setup.exe") || name.ends_with(".exe.part"))
        {
            fs::remove_file(path).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parses_prefixed_release_version() {
        assert_eq!(
            parse_release_version("v1.2.3").unwrap(),
            Version::new(1, 2, 3)
        );
        assert!(parse_release_version("release-1.2.3").is_err());
    }

    #[test]
    fn selects_exact_checksum_entry() {
        let expected = "a".repeat(64);
        let contents = format!(
            "{}  other.exe\n{}  asterline-1.2.3-x86_64-windows-setup.exe\n",
            "b".repeat(64),
            expected
        );
        assert_eq!(
            checksum_for(&contents, "asterline-1.2.3-x86_64-windows-setup.exe").unwrap(),
            expected
        );
        assert!(checksum_for(&contents, "asterline-1.2.4.exe").is_err());
    }

    #[test]
    fn checksum_rejects_non_hex_and_wrong_length() {
        assert!(checksum_for("xyz  installer.exe", "installer.exe").is_err());
        assert!(checksum_for("abcd  installer.exe", "installer.exe").is_err());
    }

    #[test]
    fn check_interval_is_bounded() {
        let root =
            std::env::temp_dir().join(format!("asterline-update-stamp-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let stamp = root.join("last-check");
        fs::write(&stamp, b"v1.2.3").unwrap();

        let modified = stamp.metadata().unwrap().modified().unwrap();
        assert!(!check_is_due(&stamp, modified + Duration::from_secs(60)));
        assert!(check_is_due(&stamp, modified + CHECK_INTERVAL));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn update_lock_excludes_competing_checks_and_recovers_after_drop() {
        let root = std::env::temp_dir().join(format!(
            "asterline-update-lock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&root).unwrap();

        let first = UpdateLock::acquire(&root).unwrap().unwrap();
        assert!(UpdateLock::acquire(&root).unwrap().is_none());
        drop(first);
        assert!(UpdateLock::acquire(&root).unwrap().is_some());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cleanup_removes_only_cached_installer_artifacts() {
        let root = std::env::temp_dir().join(format!(
            "asterline-update-cleanup-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&root).unwrap();
        let installer = root.join("asterline-1.2.3-x86_64-windows-setup.exe");
        let partial = root.join("asterline-1.2.4-x86_64-windows-setup.exe.part");
        let unrelated = root.join("keep.txt");
        fs::write(&installer, b"old").unwrap();
        fs::write(&partial, b"partial").unwrap();
        fs::write(&unrelated, b"keep").unwrap();

        cleanup_cached_installers(&root);

        assert!(!installer.exists());
        assert!(!partial.exists());
        assert!(unrelated.exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn hashes_files_with_sha256() {
        let path =
            std::env::temp_dir().join(format!("asterline-update-hash-{}", std::process::id()));
        fs::write(&path, b"hello world").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        fs::remove_file(path).ok();
    }

    #[test]
    fn downloads_only_when_checksum_matches() {
        let body = b"verified installer fixture";
        let expected = hex_digest(Sha256::digest(body).as_slice());
        let (url, server) = serve_once(body);
        let path = std::env::temp_dir().join(format!(
            "asterline-update-download-{}.exe",
            std::process::id()
        ));

        download_verified(&update_agent(), &url, &path, &expected).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&path).unwrap(), body);

        fs::remove_file(path).ok();
    }

    #[test]
    fn checksum_mismatch_leaves_no_installer() {
        let body = b"tampered installer fixture";
        let (url, server) = serve_once(body);
        let path = std::env::temp_dir().join(format!(
            "asterline-update-rejected-{}.exe",
            std::process::id()
        ));

        let result = download_verified(&update_agent(), &url, &path, &"0".repeat(64));
        server.join().unwrap();
        assert!(result.unwrap_err().contains("checksum mismatch"));
        assert!(!path.exists());
        assert!(!path.with_extension("exe.part").exists());
    }

    fn serve_once(body: &'static [u8]) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        (format!("http://{address}/asset.exe"), server)
    }
}
