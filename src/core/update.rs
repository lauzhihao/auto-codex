use std::env;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::Archive;
use uuid::Uuid;
use zip::ZipArchive;

const DEFAULT_REPO: &str = "lauzhihao/scodex";
const BINARY_NAME: &str = "scodex";
const BINARY_NAME_WIN: &str = "scodex.exe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTarget {
    pub triple: &'static str,
    pub archive_ext: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub repo: String,
    pub tag: String,
    pub version: String,
    pub target: ReleaseTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Updated,
    AlreadyCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub status: UpdateStatus,
    pub previous_version: String,
    pub installed_version: String,
    pub executable_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
}

pub fn self_update(force: bool) -> Result<UpdateOutcome> {
    let executable_path =
        env::current_exe().context("failed to resolve current executable path")?;
    let previous_version = env!("CARGO_PKG_VERSION").to_string();
    let asset = resolve_release_asset()?;

    if asset.version == previous_version && !force {
        return Ok(UpdateOutcome {
            status: UpdateStatus::AlreadyCurrent,
            previous_version: previous_version.clone(),
            installed_version: previous_version,
            executable_path,
        });
    }

    let archive_bytes = download_archive(&asset)?;
    let expected_hash = download_sha256(&asset)?;
    verify_sha256(&archive_bytes, &expected_hash)
        .with_context(|| format!("SHA-256 mismatch for {}", asset.asset_name()))?;

    let binary = extract_binary(asset.target.archive_ext, &archive_bytes)?;
    let temp_dir = env::temp_dir().join(format!("scodex-update-{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create {}", temp_dir.display()))?;
    let temp_binary = temp_dir.join(binary_filename_for_current_platform());
    fs::write(&temp_binary, &binary)
        .with_context(|| format!("failed to write {}", temp_binary.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&temp_binary)
            .with_context(|| format!("failed to stat {}", temp_binary.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&temp_binary, permissions)
            .with_context(|| format!("failed to chmod {}", temp_binary.display()))?;
    }

    update_sidecar_binaries(&executable_path, &binary)?;
    self_replace::self_replace(&temp_binary)
        .with_context(|| format!("failed to replace {}", executable_path.display()))?;
    if let Err(e) = fs::remove_dir_all(&temp_dir) {
        eprintln!(
            "warning: failed to clean temp dir {}: {e}",
            temp_dir.display()
        );
    }

    Ok(UpdateOutcome {
        status: UpdateStatus::Updated,
        previous_version,
        installed_version: asset.version,
        executable_path,
    })
}

fn resolve_release_asset() -> Result<ReleaseAsset> {
    let repo = if let Ok(r) = env::var("AUTO_CODEX_REPO") {
        eprintln!(
            "warning: download source overridden by AUTO_CODEX_REPO=\"{r}\"; \
             ensure this is a trusted repository"
        );
        r
    } else {
        DEFAULT_REPO.to_string()
    };

    let tag = if let Ok(value) = env::var("AUTO_CODEX_VERSION") {
        normalize_tag(&value)
    } else {
        fetch_latest_release_tag(&repo)?
    };
    let version = strip_tag_prefix(&tag).to_string();
    let target = detect_release_target()?;

    Ok(ReleaseAsset {
        repo,
        tag,
        version,
        target,
    })
}

fn fetch_latest_release_tag(repo: &str) -> Result<String> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("scodex"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    let client = Client::builder().default_headers(headers).build()?;
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let release = client
        .get(url)
        .send()
        .context("failed to request GitHub latest release")?
        .error_for_status()
        .context("GitHub latest release request failed")?
        .json::<GithubRelease>()
        .context("failed to decode GitHub latest release response")?;
    Ok(normalize_tag(&release.tag_name))
}

/// 下载 release 压缩包原始字节（未解压）。
fn download_archive(asset: &ReleaseAsset) -> Result<Vec<u8>> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("scodex"));
    let client = Client::builder().default_headers(headers).build()?;
    let bytes = client
        .get(asset.download_url())
        .send()
        .context("failed to download release asset")?
        .error_for_status()
        .context("release asset request failed")?
        .bytes()
        .context("failed to read release asset bytes")?;
    Ok(bytes.to_vec())
}

/// 下载 .sha256 文件并返回期望哈希字符串（hex，小写）。
///
/// .sha256 文件格式：`<hex>  <filename>` 或仅 `<hex>`（两者均支持）。
fn download_sha256(asset: &ReleaseAsset) -> Result<String> {
    let sha256_url = asset.sha256_url();
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("scodex"));
    let client = Client::builder().default_headers(headers).build()?;
    let resp = client
        .get(&sha256_url)
        .send()
        .context("failed to request .sha256 file")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "release does not provide a .sha256 file for {}; \
             cannot verify integrity - aborting update",
            asset.asset_name()
        );
    }

    let text = resp
        .error_for_status()
        .context("failed to download .sha256 file")?
        .text()
        .context("failed to read .sha256 file content")?;

    // 取第一个空白前的 token 即哈希，兼容 `<hex>  <filename>` 格式
    let hash = text
        .split_whitespace()
        .next()
        .context(".sha256 file is empty or malformed")?
        .to_lowercase();

    Ok(hash)
}

/// 对 bytes 计算 SHA-256，与 expected（hex 小写）比对。
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected.trim().to_lowercase() {
        bail!(
            "SHA-256 mismatch: expected {expected}, got {actual}; \
             the downloaded file may be corrupted or tampered"
        );
    }
    Ok(())
}

/// 从压缩包字节中提取目标二进制。
fn extract_binary(archive_ext: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    match archive_ext {
        "tar.gz" => extract_binary_from_tar_gz(bytes),
        "zip" => extract_binary_from_zip(bytes),
        other => bail!("unsupported archive extension: {other}"),
    }
}

fn extract_binary_from_tar_gz(bytes: &[u8]) -> Result<Vec<u8>> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    for entry in archive
        .entries()
        .context("failed to read tar archive entries")?
    {
        let mut entry = entry.context("failed to read tar archive entry")?;
        let path = entry.path().context("failed to read tar entry path")?;
        if path.as_ref() == Path::new(BINARY_NAME) {
            let mut contents = Vec::new();
            entry
                .read_to_end(&mut contents)
                .with_context(|| format!("failed to extract {BINARY_NAME} from tar archive"))?;
            return Ok(contents);
        }
    }
    bail!("release archive did not contain {BINARY_NAME}")
}

fn extract_binary_from_zip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).context("failed to open zip archive")?;
    let mut file = archive
        .by_name(BINARY_NAME_WIN)
        .with_context(|| format!("release archive did not contain {BINARY_NAME_WIN}"))?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .with_context(|| format!("failed to extract {BINARY_NAME_WIN} from zip archive"))?;
    Ok(contents)
}

fn update_sidecar_binaries(current_executable: &Path, binary: &[u8]) -> Result<()> {
    let Some(dir) = current_executable.parent() else {
        return Ok(());
    };
    for sibling in compatibility_binary_names() {
        let path = dir.join(sibling);
        if path == current_executable || !path.exists() {
            continue;
        }
        fs::write(&path, binary).with_context(|| format!("failed to update {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions)
                .with_context(|| format!("failed to chmod {}", path.display()))?;
        }
    }
    Ok(())
}

fn detect_release_target() -> Result<ReleaseTarget> {
    detect_release_target_for(env::consts::OS, env::consts::ARCH)
}

fn detect_release_target_for(os: &str, arch: &str) -> Result<ReleaseTarget> {
    match (os, arch) {
        ("linux", "x86_64") => Ok(ReleaseTarget {
            triple: "x86_64-unknown-linux-musl",
            archive_ext: "tar.gz",
        }),
        ("macos", "x86_64") => Ok(ReleaseTarget {
            triple: "x86_64-apple-darwin",
            archive_ext: "tar.gz",
        }),
        ("macos", "aarch64") => Ok(ReleaseTarget {
            triple: "aarch64-apple-darwin",
            archive_ext: "tar.gz",
        }),
        ("windows", "x86_64") => Ok(ReleaseTarget {
            triple: "x86_64-pc-windows-msvc",
            archive_ext: "zip",
        }),
        ("windows", "aarch64") => bail!(
            "Windows ARM64 release assets are not published yet. Build from source with cargo for now."
        ),
        _ => bail!("unsupported release target: {os}/{arch}"),
    }
}

fn normalize_tag(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with('v') {
        trimmed.to_string()
    } else {
        format!("v{trimmed}")
    }
}

fn strip_tag_prefix(value: &str) -> &str {
    value.strip_prefix('v').unwrap_or(value)
}

fn binary_filename_for_current_platform() -> &'static str {
    if cfg!(windows) {
        BINARY_NAME_WIN
    } else {
        BINARY_NAME
    }
}

fn compatibility_binary_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["auto-codex.exe"]
    } else {
        &["auto-codex"]
    }
}

impl ReleaseAsset {
    pub fn asset_name(&self) -> String {
        format!(
            "{BINARY_NAME}-{}-{}.{}",
            self.tag, self.target.triple, self.target.archive_ext
        )
    }

    pub fn download_url(&self) -> String {
        format!(
            "https://github.com/{}/releases/download/{}/{}",
            self.repo,
            self.tag,
            self.asset_name()
        )
    }

    pub fn sha256_url(&self) -> String {
        format!("{}.sha256", self.download_url())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ReleaseAsset, ReleaseTarget, detect_release_target_for, normalize_tag, strip_tag_prefix,
        verify_sha256,
    };

    #[test]
    fn release_target_mapping_matches_published_assets() {
        let linux = detect_release_target_for("linux", "x86_64").expect("linux target");
        assert_eq!(linux.triple, "x86_64-unknown-linux-musl");
        assert_eq!(linux.archive_ext, "tar.gz");

        let mac = detect_release_target_for("macos", "aarch64").expect("mac target");
        assert_eq!(mac.triple, "aarch64-apple-darwin");
        assert_eq!(mac.archive_ext, "tar.gz");

        let windows = detect_release_target_for("windows", "x86_64").expect("windows target");
        assert_eq!(windows.triple, "x86_64-pc-windows-msvc");
        assert_eq!(windows.archive_ext, "zip");
    }

    #[test]
    fn tag_normalization_is_stable() {
        assert_eq!(normalize_tag("v1.2.3"), "v1.2.3");
        assert_eq!(normalize_tag("1.2.3"), "v1.2.3");
        assert_eq!(strip_tag_prefix("v1.2.3"), "1.2.3");
    }

    #[test]
    fn release_asset_url_matches_installer_naming() {
        let asset = ReleaseAsset {
            repo: "lauzhihao/scodex".into(),
            tag: "v1.2.3".into(),
            version: "1.2.3".into(),
            target: ReleaseTarget {
                triple: "x86_64-unknown-linux-musl",
                archive_ext: "tar.gz",
            },
        };

        assert_eq!(
            asset.asset_name(),
            "scodex-v1.2.3-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset.download_url(),
            "https://github.com/lauzhihao/scodex/releases/download/v1.2.3/scodex-v1.2.3-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset.sha256_url(),
            "https://github.com/lauzhihao/scodex/releases/download/v1.2.3/scodex-v1.2.3-x86_64-unknown-linux-musl.tar.gz.sha256"
        );
    }

    #[test]
    fn verify_sha256_passes_for_correct_hash() {
        // echo -n "hello" | sha256sum => 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        let data = b"hello";
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        verify_sha256(data, expected).expect("correct hash should pass");
    }

    #[test]
    fn verify_sha256_fails_for_wrong_hash() {
        let data = b"hello";
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        let result = verify_sha256(data, wrong_hash);
        assert!(result.is_err(), "wrong hash must return Err");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("SHA-256 mismatch"),
            "error message should mention SHA-256 mismatch, got: {msg}"
        );
    }

    #[test]
    fn verify_sha256_fails_for_tampered_data() {
        use sha2::Digest;
        // 正确数据的哈希，用篡改数据验证应失败
        let correct_data = b"trusted binary";
        let mut hasher = sha2::Sha256::new();
        hasher.update(correct_data);
        let correct_hash = format!("{:x}", hasher.finalize());

        let tampered_data = b"malicious binary";
        let result = verify_sha256(tampered_data, &correct_hash);
        assert!(result.is_err(), "tampered data must fail verification");
    }
}
