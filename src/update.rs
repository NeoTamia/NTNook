//! GitHub release checks and self-replacement of the Nook binary.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_REPO: &str = "NeoTamia/NTNook";
const DEFAULT_RELEASES_URL: &str = "https://api.github.com/repos/NeoTamia/NTNook/releases/latest";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const ARCHIVE_NAME: &str = "nook-x86_64-pc-windows-msvc.zip";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const CHECKSUM_NAME: &str = "nook-x86_64-pc-windows-msvc.zip.sha256";
#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
const ARCHIVE_NAME: &str = "nook-x86_64-unknown-linux-musl.tar.xz";
#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
const CHECKSUM_NAME: &str = "nook-x86_64-unknown-linux-musl.tar.xz.sha256";
const PASSIVE_TIMEOUT: Duration = Duration::from_secs(1);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const ARCHIVE_SIZE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum Error {
    Network(String),
    Archive(String),
    Checksum(String),
    InstallMethod(String),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(message) => {
                write!(formatter, "cannot fetch Nook releases: {message}")
            }
            Self::Archive(message) => {
                write!(formatter, "cannot unpack the Nook release: {message}")
            }
            Self::Checksum(message) | Self::InstallMethod(message) => {
                write!(formatter, "{message}")
            }
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallKind {
    Cargo,
    Development,
    Managed,
    Unknown,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

struct Release {
    version: String,
    archive_url: String,
    checksum_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct UpdateCache {
    checked_at_unix_ms: u64,
    latest: String,
}

pub(crate) fn warn_if_available(errors: &mut impl Write) {
    if update_check_disabled() {
        return;
    }
    let Some(latest) = cached_or_fetch() else {
        return;
    };
    if version_is_newer(&latest, CURRENT_VERSION) {
        let _ = writeln!(
            errors,
            "warning: nook {latest} is available (installed {CURRENT_VERSION}); run `nook update`"
        );
    }
}

pub(crate) fn perform(
    check: bool,
    force: bool,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> Result<i32, Error> {
    if check {
        return check_latest(output, errors);
    }
    install_latest(force, output)
}

fn check_latest(output: &mut impl Write, errors: &mut impl Write) -> Result<i32, Error> {
    match fetch_latest(COMMAND_TIMEOUT) {
        Ok(release) => {
            let _ = store_cache(&release.version);
            if version_is_newer(&release.version, CURRENT_VERSION) {
                writeln!(
                    output,
                    "nook {} is available (installed {CURRENT_VERSION})",
                    release.version
                )?;
                Ok(1)
            } else {
                writeln!(
                    output,
                    "nook {CURRENT_VERSION} is already the latest version"
                )?;
                Ok(0)
            }
        }
        Err(error) => {
            writeln!(errors, "error: {error}")?;
            Ok(2)
        }
    }
}

fn install_latest(force: bool, output: &mut impl Write) -> Result<i32, Error> {
    if !cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64")
    )) {
        return Err(Error::InstallMethod(
            "self-update publishes x86_64 Linux and Windows binaries only; reinstall for this architecture"
                .into(),
        ));
    }
    let executable = current_executable()?;
    match install_kind(&executable) {
        InstallKind::Cargo => {
            return Err(Error::InstallMethod(
                "this nook binary was installed with cargo; update with `cargo install ntnook --locked --force`"
                    .into(),
            ));
        }
        InstallKind::Development => {
            return Err(Error::InstallMethod(format!(
                "refusing to replace a development binary at {}",
                executable.display()
            )));
        }
        InstallKind::Unknown => {
            return Err(Error::InstallMethod(format!(
                "refusing to replace {} because it was not installed by the Nook installer; re-run the installer or set NOOK_INSTALL_DIR",
                executable.display()
            )));
        }
        InstallKind::Managed => {}
    }

    let release = fetch_latest(COMMAND_TIMEOUT)?;
    let _ = store_cache(&release.version);
    if !force && !version_is_newer(&release.version, CURRENT_VERSION) {
        writeln!(
            output,
            "nook {CURRENT_VERSION} is already the latest version"
        )?;
        return Ok(0);
    }

    let archive = download_bytes(&release.archive_url, DOWNLOAD_TIMEOUT)?;
    let checksum_file = download_text(&release.checksum_url, DOWNLOAD_TIMEOUT)?;
    let expected = parse_sha256_file(&checksum_file, ARCHIVE_NAME)
        .ok_or_else(|| Error::Checksum("unable to parse the release checksum file".into()))?;
    let actual = sha256_hex(&archive);
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(Error::Checksum("release archive checksum mismatch".into()));
    }
    let binary = extract_nook(&archive)?;
    replace_binary(&executable, &binary)?;
    if release.version == CURRENT_VERSION {
        writeln!(output, "reinstalled nook {}", release.version)?;
    } else {
        writeln!(
            output,
            "updated nook from {CURRENT_VERSION} to {}",
            release.version
        )?;
    }
    Ok(0)
}

fn cached_or_fetch() -> Option<String> {
    let now = unix_ms_now();
    if let Some(cache) = load_cache()
        && cache_is_fresh(cache.checked_at_unix_ms, now)
    {
        return cached_latest(cache.latest);
    }
    match fetch_latest(PASSIVE_TIMEOUT) {
        Ok(release) => {
            let _ = store_cache(&release.version);
            Some(release.version)
        }
        Err(_) => {
            let latest = load_cache().map(|cache| cache.latest).unwrap_or_default();
            let _ = store_cache(&latest);
            cached_latest(latest)
        }
    }
}

fn cached_latest(latest: String) -> Option<String> {
    if latest.is_empty() {
        None
    } else {
        Some(latest)
    }
}

fn fetch_latest(timeout: Duration) -> Result<Release, Error> {
    let agent = http_agent(timeout);
    let url = releases_url();
    let mut response = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| Error::Network(error.to_string()))?;
    let payload: GithubRelease = response
        .body_mut()
        .read_json()
        .map_err(|error| Error::Network(error.to_string()))?;
    let version = parse_tag(&payload.tag_name).ok_or_else(|| {
        Error::Network(format!("unrecognized release tag `{}`", payload.tag_name))
    })?;
    Ok(Release {
        archive_url: asset_url(&payload, ARCHIVE_NAME)
            .unwrap_or_else(|| github_download_url(&version, ARCHIVE_NAME)),
        checksum_url: asset_url(&payload, CHECKSUM_NAME)
            .unwrap_or_else(|| github_download_url(&version, CHECKSUM_NAME)),
        version,
    })
}

fn download_bytes(url: &str, timeout: Duration) -> Result<Vec<u8>, Error> {
    let agent = http_agent(timeout);
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| Error::Network(error.to_string()))?;
    response
        .body_mut()
        .with_config()
        .limit(ARCHIVE_SIZE_LIMIT)
        .read_to_vec()
        .map_err(|error| Error::Network(error.to_string()))
}

fn download_text(url: &str, timeout: Duration) -> Result<String, Error> {
    let bytes = download_bytes(url, timeout)?;
    String::from_utf8(bytes)
        .map_err(|error| Error::Checksum(format!("checksum file is not UTF-8: {error}")))
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(format!("nook/{CURRENT_VERSION}"))
        .build()
        .into()
}

#[cfg(not(windows))]
fn extract_nook(archive: &[u8]) -> Result<Vec<u8>, Error> {
    let mut decompressed = LimitedBuffer::new(ARCHIVE_SIZE_LIMIT);
    lzma_rs::xz_decompress(&mut Cursor::new(archive), &mut decompressed)
        .map_err(|error| Error::Archive(error.to_string()))?;
    let mut tar = tar::Archive::new(Cursor::new(decompressed.data));
    for entry in tar
        .entries()
        .map_err(|error| Error::Archive(error.to_string()))?
    {
        let mut entry = entry.map_err(|error| Error::Archive(error.to_string()))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| Error::Archive(error.to_string()))?;
        if path.file_name() != Some(OsStr::new("nook")) {
            continue;
        }
        return read_limited(&mut entry, ARCHIVE_SIZE_LIMIT)
            .map_err(|error| Error::Archive(error.to_string()));
    }
    Err(Error::Archive(
        "archive does not contain a nook binary".into(),
    ))
}

#[cfg(windows)]
fn extract_nook(archive: &[u8]) -> Result<Vec<u8>, Error> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|error| Error::Archive(error.to_string()))?;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| Error::Archive(error.to_string()))?;
        if entry.is_file() && Path::new(entry.name()).file_name() == Some(OsStr::new("nook.exe")) {
            return read_limited(&mut entry, ARCHIVE_SIZE_LIMIT)
                .map_err(|error| Error::Archive(error.to_string()));
        }
    }
    Err(Error::Archive(
        "archive does not contain a nook.exe binary".into(),
    ))
}

#[cfg(not(windows))]
struct LimitedBuffer {
    data: Vec<u8>,
    limit: u64,
}

#[cfg(not(windows))]
impl LimitedBuffer {
    fn new(limit: u64) -> Self {
        Self {
            data: Vec::new(),
            limit,
        }
    }
}

#[cfg(not(windows))]
impl Write for LimitedBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let used = u64::try_from(self.data.len()).unwrap_or(u64::MAX);
        let added = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if used.saturating_add(added) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decompressed archive exceeds size limit",
            ));
        }
        self.data.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_limited(reader: &mut impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "nook binary exceeds size limit",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn replace_binary(destination: &Path, contents: &[u8]) -> Result<(), Error> {
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "binary path has no parent directory",
        )
    })?;
    let name = destination.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "binary path has no file name")
    })?;
    let temporary = parent.join(format!(
        ".{}.update.{}.tmp",
        name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o755);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(destination)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0o755);
            fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        }
        crate::platform::replace_file(&temporary, destination)?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
const WINDOWS_UPDATE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$parent = Get-Process -Id ([int]$env:NOOK_UPDATE_PARENT_PID) -ErrorAction SilentlyContinue
if ($null -ne $parent) {
    $parent | Wait-Process
}
function Get-NookPathIdentity([string]$path) {
    $full = [IO.Path]::GetFullPath($path)
    if ($full.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
        return '\\' + $full.Substring(8)
    }
    if ($full.StartsWith('\\?\', [StringComparison]::OrdinalIgnoreCase)) {
        return $full.Substring(4)
    }
    return $full
}
$destination = [IO.Path]::GetFullPath($env:NOOK_UPDATE_DESTINATION)
$destinationIdentity = Get-NookPathIdentity $destination
$blockers = @(
    Get-Process -ErrorAction SilentlyContinue | Where-Object {
        if ($_.Id -eq $PID) {
            return $false
        }
        try {
            $null -ne $_.Path -and
                [string]::Equals(
                    (Get-NookPathIdentity $_.Path),
                    $destinationIdentity,
                    [StringComparison]::OrdinalIgnoreCase
                )
        } catch {
            $false
        }
    }
)
if ($blockers.Count -gt 0) {
    $blockers | Wait-Process
}
try {
    Move-Item -LiteralPath $env:NOOK_UPDATE_SOURCE -Destination $destination -Force
} catch {
    Remove-Item -LiteralPath $env:NOOK_UPDATE_SOURCE -Force -ErrorAction SilentlyContinue
    throw
}
"#;

#[cfg(windows)]
fn replace_binary(destination: &Path, contents: &[u8]) -> Result<(), Error> {
    use std::process::{Command, Stdio};

    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "binary path has no parent directory",
        )
    })?;
    let temporary = parent.join(format!("nook.update.{}.exe", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    let result = (|| {
        let mut file = options.write(true).create_new(true).open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);

        Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                WINDOWS_UPDATE_SCRIPT,
            ])
            .env("NOOK_UPDATE_PARENT_PID", std::process::id().to_string())
            .env("NOOK_UPDATE_SOURCE", &temporary)
            .env("NOOK_UPDATE_DESTINATION", destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn current_executable() -> Result<PathBuf, Error> {
    let path = env::current_exe()?;
    Ok(path.canonicalize().unwrap_or(path))
}

fn install_kind(path: &Path) -> InstallKind {
    if is_cargo_install(path) {
        InstallKind::Cargo
    } else if is_development_binary(path) {
        InstallKind::Development
    } else if is_managed_install(path) {
        InstallKind::Managed
    } else {
        InstallKind::Unknown
    }
}

fn is_cargo_install(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if parent.file_name() != Some(OsStr::new("bin")) {
        return false;
    }
    if let Ok(cargo_home) = env::var("CARGO_HOME")
        && parent == Path::new(&cargo_home).join("bin")
    {
        return true;
    }
    parent
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == ".cargo")
}

fn is_managed_install(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    is_default_user_bin(parent)
        || installer_directories()
            .iter()
            .any(|directory| paths_match(parent, directory))
}

#[cfg(unix)]
fn is_default_user_bin(parent: &Path) -> bool {
    let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) else {
        return false;
    };
    paths_match(parent, &PathBuf::from(home).join(".local/bin"))
}

#[cfg(windows)]
fn is_default_user_bin(parent: &Path) -> bool {
    env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .is_some_and(|local| paths_match(parent, &local.join("Programs/Nook/bin")))
}

fn installer_directories() -> Vec<PathBuf> {
    ["NOOK_INSTALL_DIR", "XDG_BIN_HOME"]
        .into_iter()
        .filter_map(|key| env::var_os(key).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .collect()
}

fn paths_match(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

fn is_development_binary(path: &Path) -> bool {
    let mut saw_target = false;
    for component in path.components() {
        if component.as_os_str() == "target" {
            saw_target = true;
        } else if saw_target
            && (component.as_os_str() == "debug" || component.as_os_str() == "release")
        {
            return true;
        }
    }
    false
}

fn releases_url() -> String {
    env::var("NOOK_UPDATE_RELEASES_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASES_URL.to_owned())
}

fn asset_url(release: &GithubRelease, name: &str) -> Option<String> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .map(|asset| asset.browser_download_url.clone())
}

fn github_download_url(version: &str, name: &str) -> String {
    format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/{name}")
}

fn update_check_disabled() -> bool {
    env::var_os("NOOK_DISABLE_UPDATE_CHECK").is_some_and(|value| value != "0")
}

#[cfg(unix)]
fn cache_path() -> Option<PathBuf> {
    if let Some(directory) = env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory).join("nook/update-check.json"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".cache/nook/update-check.json"))
}

#[cfg(windows)]
fn cache_path() -> Option<PathBuf> {
    if let Some(directory) = env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory).join("nook/update-check.json"));
    }
    env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(|directory| PathBuf::from(directory).join("Nook/cache/update-check.json"))
}

fn load_cache() -> Option<UpdateCache> {
    let contents = fs::read(cache_path()?).ok()?;
    serde_json::from_slice(&contents).ok()
}

fn store_cache(latest: &str) -> io::Result<()> {
    let Some(path) = cache_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let cache = UpdateCache {
        checked_at_unix_ms: unix_ms_now(),
        latest: latest.to_owned(),
    };
    let contents = serde_json::to_vec(&cache).map_err(io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, contents)?;
    crate::platform::replace_file(&temporary, &path)?;
    Ok(())
}

fn cache_is_fresh(checked_at_unix_ms: u64, now_ms: u64) -> bool {
    let ttl_ms = u64::try_from(CACHE_TTL.as_millis()).unwrap_or(u64::MAX);
    now_ms.saturating_sub(checked_at_unix_ms) < ttl_ms
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn parse_tag(tag: &str) -> Option<String> {
    parse_version(tag).map(|version| version.to_string())
}

fn parse_version(input: &str) -> Option<Version> {
    let input = input.trim();
    let input = input.strip_prefix('v').unwrap_or(input);
    let mut parts = input.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
    })
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn parse_sha256_file(contents: &str, archive_name: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        match parts.next() {
            None => return Some(hash.to_ascii_lowercase()),
            Some(name) => {
                let name = name.trim_start_matches('*');
                if name == archive_name
                    || Path::new(name).file_name() == Some(OsStr::new(archive_name))
                {
                    return Some(hash.to_ascii_lowercase());
                }
            }
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::{InstallKind, LimitedBuffer, install_kind, read_limited};
    use super::{cache_is_fresh, parse_sha256_file, parse_version, version_is_newer};
    #[cfg(unix)]
    use std::env;
    #[cfg(unix)]
    use std::io::{self, Cursor, Write};
    #[cfg(unix)]
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[cfg(windows)]
    #[test]
    fn deferred_windows_update_tolerates_an_absent_parent() {
        use std::process::Command;

        let directory = std::env::temp_dir().join(format!("nook-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.exe");
        let destination = directory.join("destination.exe");
        std::fs::write(&source, b"replacement").unwrap();

        let output = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                super::WINDOWS_UPDATE_SCRIPT,
            ])
            .env("NOOK_UPDATE_PARENT_PID", "2147483647")
            .env("NOOK_UPDATE_SOURCE", &source)
            .env("NOOK_UPDATE_DESTINATION", &destination)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "PowerShell updater failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"replacement");
        assert!(!source.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn deferred_windows_update_waits_for_an_active_destination() {
        use std::process::{Command, Stdio};

        let directory = std::env::temp_dir().join(format!("nook-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.exe");
        let destination = directory.join("nook.exe");
        std::fs::write(&source, b"replacement").unwrap();
        std::fs::copy(
            std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()),
            &destination,
        )
        .unwrap();

        let mut blocker = Command::new(&destination)
            .args(["/D", "/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut updater = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                super::WINDOWS_UPDATE_SCRIPT,
            ])
            .env("NOOK_UPDATE_PARENT_PID", "2147483647")
            .env("NOOK_UPDATE_SOURCE", &source)
            .env(
                "NOOK_UPDATE_DESTINATION",
                destination.canonicalize().unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        std::thread::sleep(Duration::from_millis(500));
        assert!(updater.try_wait().unwrap().is_none());
        assert!(source.exists());

        blocker.kill().unwrap();
        blocker.wait().unwrap();
        let output = updater.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "PowerShell updater failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"replacement");
        assert!(!source.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn parses_release_tags_and_compares_semver() {
        let version = parse_version("v1.2.3").unwrap();
        assert_eq!(version.to_string(), "1.2.3");
        assert!(version_is_newer("0.4.0", "0.3.0"));
        assert!(!version_is_newer("0.3.0", "0.3.0"));
        assert!(!version_is_newer("0.2.9", "0.3.0"));
        assert!(parse_version("1.2.3-rc.1").is_none());
    }

    #[test]
    fn parses_sha256sum_files() {
        let hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let named = format!("{hash}  nook-x86_64-unknown-linux-musl.tar.xz\n");
        assert_eq!(
            parse_sha256_file(&named, "nook-x86_64-unknown-linux-musl.tar.xz").as_deref(),
            Some(hash)
        );
        let binary = format!("{hash} *nook-x86_64-unknown-linux-musl.tar.xz\n");
        assert_eq!(
            parse_sha256_file(&binary, "nook-x86_64-unknown-linux-musl.tar.xz").as_deref(),
            Some(hash)
        );
        assert_eq!(
            parse_sha256_file(hash, "nook-x86_64-unknown-linux-musl.tar.xz").as_deref(),
            Some(hash)
        );
        assert!(parse_sha256_file("not-a-hash  file", "file").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn detects_cargo_development_and_managed_installs() {
        assert_eq!(
            install_kind(Path::new("/home/user/.cargo/bin/nook")),
            InstallKind::Cargo
        );
        assert_eq!(
            install_kind(Path::new("/repo/target/debug/nook")),
            InstallKind::Development
        );
        assert_eq!(
            install_kind(Path::new(
                "/repo/target/x86_64-unknown-linux-musl/release/nook"
            )),
            InstallKind::Development
        );
        let home = env::var("HOME").expect("HOME must be set");
        assert_eq!(
            install_kind(&PathBuf::from(home).join(".local/bin/nook")),
            InstallKind::Managed
        );
        assert_eq!(
            install_kind(Path::new("/opt/tools/bin/nook")),
            InstallKind::Unknown
        );
        assert_eq!(
            install_kind(Path::new("/opt/other/.local/bin/nook")),
            InstallKind::Unknown
        );
    }

    #[test]
    #[cfg(unix)]
    fn extraction_limits_reject_oversized_output() {
        let mut writer = LimitedBuffer::new(4);
        assert!(writer.write_all(b"abcd").is_ok());
        assert_eq!(
            writer.write_all(b"e").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let within = read_limited(&mut Cursor::new(b"nook"), 4).unwrap();
        assert_eq!(within, b"nook");
        assert_eq!(
            read_limited(&mut Cursor::new(b"nook!"), 4)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn cache_freshness_uses_a_twenty_four_hour_window() {
        let ttl_ms = u64::try_from(Duration::from_secs(24 * 60 * 60).as_millis()).unwrap();
        assert!(cache_is_fresh(1_000, 1_000));
        assert!(cache_is_fresh(1_000, 1_000 + ttl_ms - 1));
        assert!(!cache_is_fresh(1_000, 1_000 + ttl_ms));
    }
}
