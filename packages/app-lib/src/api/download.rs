//! Downloads a CloudCraft release installer for the manual update flow.
//!
//! Safety rules:
//! - Only `https://github.com/Shubhajitro90/CloudCraft/releases/download/<tag>/<file>`
//!   is accepted, and redirects may only go to GitHub's release asset hosts.
//! - The file name must be a plain installer name (no path separators) that
//!   matches the URL, so nothing can be written outside the downloads folder.
//! - Every installer must be listed in the release's `SHA256SUMS.txt`. The
//!   download is streamed to a `.part` file, size-capped, hashed, and renamed
//!   into place only when the SHA-256 matches. A mismatch deletes the file.
//! - The launcher is never terminated and the installer is never executed
//!   directly; the verified file is revealed/opened for the user to run.

use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;

pub const RELEASE_DOWNLOAD_PREFIX: &str =
    "https://github.com/Shubhajitro90/CloudCraft/releases/download/";
pub const CHECKSUM_FILE: &str = "SHA256SUMS.txt";
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: usize = 64 * 1024;
const ALLOWED_HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const INSTALLER_EXTENSIONS: [&str; 6] =
    [".dmg", ".msi", ".exe", ".AppImage", ".deb", ".rpm"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub tag: String,
    pub file_name: String,
}

impl ReleaseAsset {
    pub fn download_url(&self) -> String {
        format!("{RELEASE_DOWNLOAD_PREFIX}{}/{}", self.tag, self.file_name)
    }

    pub fn checksum_url(&self) -> String {
        format!("{RELEASE_DOWNLOAD_PREFIX}{}/{CHECKSUM_FILE}", self.tag)
    }
}

pub fn validate_installer_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 200 {
        return Err("The installer file name is empty or too long.".into());
    }
    if name.starts_with('.')
        || !name.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')
        })
    {
        return Err(format!(
            "Refusing installer file name {name:?}: only letters, digits, '.', '-', '_' and '+' are allowed."
        ));
    }
    if !INSTALLER_EXTENSIONS.iter().any(|ext| name.ends_with(ext)) {
        return Err(format!(
            "Refusing {name:?}: it is not a CloudCraft installer type."
        ));
    }
    Ok(())
}

fn validate_tag(tag: &str) -> Result<(), String> {
    if tag.is_empty()
        || tag.len() > 100
        || tag == "."
        || tag == ".."
        || !tag.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')
        })
    {
        return Err(format!("Refusing release tag {tag:?}."));
    }
    Ok(())
}

pub fn parse_release_asset_url(url: &str) -> Result<ReleaseAsset, String> {
    let rest = url.strip_prefix(RELEASE_DOWNLOAD_PREFIX).ok_or_else(|| {
        "Updates may only be downloaded from CloudCraft's GitHub releases."
            .to_string()
    })?;
    if rest.contains(['?', '#', '\\', '%']) {
        return Err(
            "The release download URL contains unsupported characters.".into(),
        );
    }
    let mut parts = rest.split('/');
    let (Some(tag), Some(file_name), None) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Err("The release download URL must be <tag>/<file>.".into());
    };
    validate_tag(tag)?;
    validate_installer_name(file_name)?;
    Ok(ReleaseAsset {
        tag: tag.to_string(),
        file_name: file_name.to_string(),
    })
}

/// Finds `file_name` in `sha256sum`-format text (`<hex>  <name>` or `<hex> *<name>`).
pub fn parse_checksum(text: &str, file_name: &str) -> Result<[u8; 32], String> {
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((hash, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim_start().trim_start_matches('*');
        if name != file_name {
            continue;
        }
        let bytes = hex::decode(hash.trim()).map_err(|_| {
            format!("{CHECKSUM_FILE} has an invalid entry for {file_name}.")
        })?;
        return bytes.try_into().map_err(|_| {
            format!("{CHECKSUM_FILE} has a non-SHA-256 entry for {file_name}.")
        });
    }
    Err(format!(
        "{file_name} is not listed in the release's {CHECKSUM_FILE}, so it cannot be verified."
    ))
}

fn release_client() -> Result<reqwest::Client, String> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let allowed = attempt.url().scheme() == "https"
            && attempt
                .url()
                .host_str()
                .is_some_and(|host| ALLOWED_HOSTS.contains(&host));
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if allowed {
            attempt.follow()
        } else {
            attempt.error("redirect to an untrusted host was refused")
        }
    });
    reqwest::Client::builder()
        .redirect(policy)
        .https_only(true)
        .connect_timeout(Duration::from_secs(20))
        .user_agent(concat!("CloudCraft-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("Could not create the download client: {e}"))
}

async fn fetch_checksums(
    client: &reqwest::Client,
    url: &str,
) -> Result<String, String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| format!("Could not download {CHECKSUM_FILE}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "The release has no {CHECKSUM_FILE} (HTTP {}), so its installer cannot be verified.",
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Could not read {CHECKSUM_FILE}: {e}"))?;
    if bytes.len() > MAX_CHECKSUM_BYTES {
        return Err(format!("{CHECKSUM_FILE} is unexpectedly large."));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| format!("{CHECKSUM_FILE} is not valid text."))
}

/// Streams `url` into `dest_dir/file_name` and keeps it only if its SHA-256 matches.
pub async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    expected: [u8; 32],
    dest_dir: &Path,
    file_name: &str,
    max_bytes: u64,
) -> Result<PathBuf, String> {
    validate_installer_name(file_name)?;
    fs::create_dir_all(dest_dir)
        .await
        .map_err(|e| format!("Could not create {}: {e}", dest_dir.display()))?;
    let final_path = dest_dir.join(file_name);
    let part_path = dest_dir.join(format!("{file_name}.part"));

    let result = async {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Download failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("Download failed: HTTP {}", response.status()));
        }
        if response.content_length().is_some_and(|len| len > max_bytes) {
            return Err("The installer is larger than the allowed size.".into());
        }
        let mut file = File::create(&part_path)
            .await
            .map_err(|e| format!("Could not create the download file: {e}"))?;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("Download interrupted: {e}"))?;
            written += chunk.len() as u64;
            if written > max_bytes {
                return Err("The installer is larger than the allowed size.".into());
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("Could not write the download: {e}"))?;
        }
        file.flush()
            .await
            .map_err(|e| format!("Could not write the download: {e}"))?;
        file.sync_all()
            .await
            .map_err(|e| format!("Could not write the download: {e}"))?;
        drop(file);
        let actual: [u8; 32] = hasher.finalize().into();
        if actual != expected {
            return Err(format!(
                "Checksum mismatch for {file_name}: expected {}, got {}. The file was deleted.",
                hex::encode(expected),
                hex::encode(actual)
            ));
        }
        fs::rename(&part_path, &final_path)
            .await
            .map_err(|e| format!("Could not save the installer: {e}"))?;
        Ok(final_path.clone())
    }
    .await;

    if result.is_err() {
        let _ = fs::remove_file(&part_path).await;
    }
    result
}

/// Downloads and verifies a CloudCraft release installer into `dest_dir`.
pub async fn download_release_installer(
    download_url: &str,
    local_filename: &str,
    dest_dir: &Path,
) -> Result<PathBuf, String> {
    let asset = parse_release_asset_url(download_url)?;
    if asset.file_name != local_filename {
        return Err(
            "The installer name does not match the download URL.".into()
        );
    }
    let client = release_client()?;
    let checksums = fetch_checksums(&client, &asset.checksum_url()).await?;
    let expected = parse_checksum(&checksums, &asset.file_name)?;
    download_verified(
        &client,
        &asset.download_url(),
        expected,
        dest_dir,
        &asset.file_name,
        MAX_INSTALLER_BYTES,
    )
    .await
}

async fn reveal(path: &Path, os_type: &str) -> Result<(), String> {
    let os = os_type.to_ascii_lowercase();
    let mut command = if os == "windows" {
        let mut c = tokio::process::Command::new("explorer");
        c.arg(format!("/select,{}", path.display()));
        c
    } else if os == "macos" {
        let mut c = tokio::process::Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = tokio::process::Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    command.spawn().map(|_| ()).map_err(|e| {
        format!("Downloaded and verified, but could not open it: {e}")
    })
}

/// Entry point for the `get_artifact` command. Returns the verified installer path.
pub async fn init_download(
    download_url: &str,
    local_filename: &str,
    os_type: &str,
    auto_update_supported: bool,
) -> crate::Result<PathBuf> {
    let to_error =
        |message: String| crate::ErrorKind::OtherError(message).as_error();
    let download_dir = dirs::download_dir().ok_or_else(|| {
        to_error("Could not find the Downloads folder.".into())
    })?;
    let path =
        download_release_installer(download_url, local_filename, &download_dir)
            .await
            .map_err(to_error)?;
    tracing::info!("Verified CloudCraft installer saved to {}", path.display());
    if auto_update_supported {
        reveal(&path, os_type).await.map_err(to_error)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn accepts_only_cloudcraft_release_assets() {
        let ok = parse_release_asset_url(
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v0.9.204/CloudCraft_0.9.204_amd64.AppImage",
        )
        .unwrap();
        assert_eq!(ok.tag, "v0.9.204");
        assert_eq!(ok.file_name, "CloudCraft_0.9.204_amd64.AppImage");
        assert_eq!(
            ok.checksum_url(),
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v0.9.204/SHA256SUMS.txt"
        );

        for bad in [
            "http://github.com/Shubhajitro90/CloudCraft/releases/download/v1/a.msi",
            "https://github.com/someone/CloudCraft/releases/download/v1/a.msi",
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/../a.msi",
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v1/../../a.msi",
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v1/a.msi?x=1",
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v1/%2e%2e%2fa.msi",
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v1/a.sh",
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v1/",
            "https://github.com/Shubhajitro90/CloudCraft.evil.com/releases/download/v1/a.msi",
        ] {
            assert!(parse_release_asset_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn installer_names_cannot_escape_the_folder() {
        assert!(
            validate_installer_name("CloudCraft_0.9.204_x64-setup.exe").is_ok()
        );
        for bad in [
            "../a.exe",
            "a/b.exe",
            "a\\b.exe",
            ".hidden.exe",
            "a.exe ",
            "a",
            "",
        ] {
            assert!(validate_installer_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_sha256sum_lines() {
        let hash = "a".repeat(64);
        let text = format!(
            "{hash}  CloudCraft.msi\r\n{}  *Other.dmg\n",
            "b".repeat(64)
        );
        assert_eq!(
            parse_checksum(&text, "CloudCraft.msi").unwrap(),
            [0xaa; 32]
        );
        assert_eq!(parse_checksum(&text, "Other.dmg").unwrap(), [0xbb; 32]);
        assert!(parse_checksum(&text, "Missing.exe").is_err());
        assert!(parse_checksum("zz  CloudCraft.msi", "CloudCraft.msi").is_err());
        assert!(
            parse_checksum("abcd  CloudCraft.msi", "CloudCraft.msi").is_err()
        );
    }

    async fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        url
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("cloudcraft-download-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn verified_download_is_kept_and_mismatch_is_deleted() {
        let body = b"pretend installer bytes".to_vec();
        let expected: [u8; 32] = Sha256::digest(&body).into();
        let url = serve_once(body.clone()).await;
        let client = reqwest::Client::new();
        let dir = temp_dir();

        let path = download_verified(
            &client,
            &url,
            expected,
            &dir,
            "CloudCraft.msi",
            1024,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);

        let err =
            download_verified(&client, &url, [0u8; 32], &dir, "Bad.msi", 1024)
                .await
                .unwrap_err();
        assert!(err.contains("Checksum mismatch"), "{err}");
        assert!(!dir.join("Bad.msi").exists());
        assert!(!dir.join("Bad.msi.part").exists());

        let err =
            download_verified(&client, &url, expected, &dir, "Big.msi", 4)
                .await
                .unwrap_err();
        assert!(err.contains("larger"), "{err}");
        assert!(!dir.join("Big.msi.part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Serves `status` with `body`, declaring `declared_len` bytes. A declared
    /// length larger than the body simulates a connection dropped mid-download.
    async fn serve_raw(
        status: &'static str,
        body: Vec<u8>,
        declared_len: usize,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let header = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {declared_len}\r\nConnection: close\r\n\r\n"
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        url
    }

    fn no_leftovers(dir: &Path) {
        let names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.is_empty(), "left files behind: {names:?}");
    }

    #[tokio::test]
    async fn missing_checksum_file_blocks_the_update() {
        let url = serve_raw("404 Not Found", b"Not Found".to_vec(), 9).await;
        let err = fetch_checksums(&reqwest::Client::new(), &url)
            .await
            .unwrap_err();
        assert!(err.contains("has no SHA256SUMS.txt"), "{err}");
        assert!(err.contains("404"), "{err}");
    }

    #[tokio::test]
    async fn oversized_or_binary_checksum_file_is_refused() {
        let big = vec![b'a'; MAX_CHECKSUM_BYTES + 1];
        let url = serve_raw("200 OK", big.clone(), big.len()).await;
        let err = fetch_checksums(&reqwest::Client::new(), &url)
            .await
            .unwrap_err();
        assert!(err.contains("unexpectedly large"), "{err}");

        let binary = vec![0xff, 0xfe, 0x00, 0x80];
        let url = serve_raw("200 OK", binary, 4).await;
        let err = fetch_checksums(&reqwest::Client::new(), &url)
            .await
            .unwrap_err();
        assert!(err.contains("not valid text"), "{err}");
    }

    #[test]
    fn installer_absent_from_checksums_cannot_be_verified() {
        let text =
            format!("{}  CloudCraft_1.0.0_x64-setup.exe\n", "c".repeat(64));
        let err = parse_checksum(&text, "CloudCraft_1.0.0_amd64.AppImage")
            .unwrap_err();
        assert!(err.contains("is not listed"), "{err}");
        // A name that merely contains the wanted name must not match.
        let text = format!("{}  evil-CloudCraft.msi\n", "d".repeat(64));
        assert!(parse_checksum(&text, "CloudCraft.msi").is_err());
    }

    #[tokio::test]
    async fn server_error_and_interrupted_download_leave_nothing_behind() {
        let client = reqwest::Client::new();
        let body = b"partial installer".to_vec();
        let expected: [u8; 32] = Sha256::digest(&body).into();

        let dir = temp_dir();
        let url =
            serve_raw("500 Internal Server Error", b"oops".to_vec(), 4).await;
        let err =
            download_verified(&client, &url, expected, &dir, "A.msi", 1 << 20)
                .await
                .unwrap_err();
        assert!(err.contains("HTTP 500"), "{err}");
        no_leftovers(&dir);

        let url = serve_raw("200 OK", body.clone(), body.len() + 1000).await;
        let err =
            download_verified(&client, &url, expected, &dir, "B.msi", 1 << 20)
                .await
                .unwrap_err();
        assert!(err.contains("interrupted"), "{err}");
        no_leftovers(&dir);

        let declared_too_big = serve_raw("200 OK", body.clone(), 1 << 21).await;
        let err = download_verified(
            &client,
            &declared_too_big,
            expected,
            &dir,
            "C.msi",
            1 << 20,
        )
        .await
        .unwrap_err();
        assert!(err.contains("larger than the allowed size"), "{err}");
        no_leftovers(&dir);

        let err = download_verified(
            &client,
            "http://127.0.0.1:9/file",
            expected,
            &dir,
            "D.msi",
            1 << 20,
        )
        .await
        .unwrap_err();
        assert!(err.contains("Download failed"), "{err}");
        no_leftovers(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn unsafe_destination_name_is_refused_before_any_write() {
        let dir = temp_dir();
        let err = download_verified(
            &reqwest::Client::new(),
            "http://127.0.0.1:9/file",
            [0; 32],
            &dir,
            "../escape.msi",
            10,
        )
        .await
        .unwrap_err();
        assert!(err.contains("Refusing"), "{err}");
        no_leftovers(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn release_client_refuses_plain_http() {
        let client = release_client().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(client.get("http://github.com/").send())
            .unwrap_err();
        assert!(
            err.is_builder() || err.to_string().contains("scheme"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn mismatched_local_name_is_refused_before_network() {
        let err = download_release_installer(
            "https://github.com/Shubhajitro90/CloudCraft/releases/download/v1/CloudCraft.msi",
            "Other.msi",
            &std::env::temp_dir(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("does not match"), "{err}");
    }
}
