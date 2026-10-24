//! Discovery, download and integrity verification of Minecraft server software.
//!
//! Only publishers that expose a checksum for every artifact are supported:
//! Mojang's vanilla server (SHA-1) and PaperMC's Paper (SHA-256). Every
//! download is streamed into a hidden `.part` file, checked against the
//! publisher's checksum, size and JAR signature, and renamed into place only
//! at the commit point. Network access is limited to an explicit allow-list of
//! official hosts over HTTPS, including across redirects.

use crate::cloudcraft_server_backup::{real_directory, valid_component};
use crate::cloudcraft_server_ops::{OpError, Tracker};
use futures::StreamExt;
use reqwest::{redirect, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

const VANILLA_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const PAPER_API: &str = "https://fill.papermc.io/v3/projects/paper";
const ALLOWED_HOSTS: [&str; 4] = [
    "piston-meta.mojang.com",
    "piston-data.mojang.com",
    "fill.papermc.io",
    "fill-data.papermc.io",
];
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_JAR_BYTES: u64 = 512 * 1024 * 1024;
const IDLE_LIMIT: Duration = Duration::from_secs(30);
const PART_PREFIX: &str = ".cloudcraft-download-";
const JAR_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SoftwareKind {
    Vanilla,
    Paper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    Sha1,
    Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSoftware {
    pub kind: SoftwareKind,
    pub version: String,
    pub build: Option<u64>,
    pub file_name: String,
    pub url: String,
    pub size: Option<u64>,
    pub algorithm: Algorithm,
    pub checksum: String,
    pub java_major: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadedSoftware {
    pub path: String,
    pub file_name: String,
    pub bytes: u64,
    pub algorithm: Algorithm,
    pub checksum: String,
    pub reused: bool,
}

enum Hasher {
    Sha1(sha1_smol::Sha1),
    Sha256(Sha256),
}

impl Hasher {
    fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Sha1 => Self::Sha1(sha1_smol::Sha1::new()),
            Algorithm::Sha256 => Self::Sha256(Sha256::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hasher) => hasher.update(bytes),
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    fn finish(self) -> String {
        match self {
            Self::Sha1(hasher) => hasher.digest().to_string(),
            Self::Sha256(hasher) => hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }
}

/// Version ids end up in URL paths and file names, so only a conservative
/// character set is accepted.
pub fn valid_version_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !id.starts_with('.')
}

pub fn is_allowed_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url
            .host_str()
            .is_some_and(|host| ALLOWED_HOSTS.contains(&host))
}

fn valid_checksum(algorithm: Algorithm, value: &str) -> bool {
    let length = match algorithm {
        Algorithm::Sha1 => 40,
        Algorithm::Sha256 => 64,
    };
    value.len() == length
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

fn parse_json(text: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(text)
        .map_err(|error| format!("The {what} could not be understood: {error}"))
}

/// Release versions in the order Mojang publishes them (newest first).
pub fn parse_vanilla_versions(text: &str) -> Result<Vec<String>, String> {
    let value = parse_json(text, "Mojang version list")?;
    let versions = value
        .get("versions")
        .and_then(Value::as_array)
        .ok_or("The Mojang version list has no versions.")?;
    Ok(versions
        .iter()
        .filter(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("release")
        })
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .filter(|id| valid_version_id(id))
        .map(str::to_string)
        .collect())
}

/// The metadata URL and SHA-1 Mojang lists for one version.
pub fn parse_vanilla_entry(
    text: &str,
    version: &str,
) -> Result<(String, String), String> {
    let value = parse_json(text, "Mojang version list")?;
    let entry = value
        .get("versions")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter().find(|entry| {
                entry.get("id").and_then(Value::as_str) == Some(version)
                    && entry.get("type").and_then(Value::as_str)
                        == Some("release")
            })
        })
        .ok_or_else(|| {
            format!("Minecraft {version} is not a released vanilla version.")
        })?;
    let url = entry.get("url").and_then(Value::as_str);
    let sha1 = entry.get("sha1").and_then(Value::as_str);
    match (url, sha1) {
        (Some(url), Some(sha1)) if valid_checksum(Algorithm::Sha1, sha1) => {
            Ok((url.to_string(), sha1.to_string()))
        }
        _ => Err(format!(
            "Mojang's entry for {version} has no usable checksum."
        )),
    }
}

pub fn parse_vanilla_package(
    text: &str,
    version: &str,
) -> Result<ResolvedSoftware, String> {
    let value = parse_json(text, "Mojang version metadata")?;
    let server = value
        .get("downloads")
        .and_then(|downloads| downloads.get("server"))
        .ok_or_else(|| {
            format!("Minecraft {version} has no downloadable server.")
        })?;
    let url = server.get("url").and_then(Value::as_str);
    let sha1 = server.get("sha1").and_then(Value::as_str);
    let (Some(url), Some(sha1)) = (url, sha1) else {
        return Err(format!("The server entry for {version} is incomplete."));
    };
    if !valid_checksum(Algorithm::Sha1, sha1) {
        return Err(format!("The server checksum for {version} is invalid."));
    }
    Ok(ResolvedSoftware {
        kind: SoftwareKind::Vanilla,
        version: version.to_string(),
        build: None,
        file_name: format!("minecraft_server.{version}.jar"),
        url: url.to_string(),
        size: server.get("size").and_then(Value::as_u64),
        algorithm: Algorithm::Sha1,
        checksum: sha1.to_string(),
        java_major: value
            .get("javaVersion")
            .and_then(|java| java.get("majorVersion"))
            .and_then(Value::as_u64)
            .and_then(|major| u32::try_from(major).ok()),
    })
}

fn version_key(id: &str) -> Vec<u64> {
    id.split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

/// Stable Paper versions, newest first. Pre-releases and release candidates
/// (anything with a suffix) are left out.
pub fn parse_paper_versions(text: &str) -> Result<Vec<String>, String> {
    let value = parse_json(text, "Paper version list")?;
    let groups = value
        .get("versions")
        .and_then(Value::as_object)
        .ok_or("The Paper version list has no versions.")?;
    let mut versions: Vec<String> = groups
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| valid_version_id(id) && !id.contains('-'))
        .map(str::to_string)
        .collect();
    versions.sort_by_key(|id| std::cmp::Reverse(version_key(id)));
    versions.dedup();
    Ok(versions)
}

pub fn parse_paper_build(
    text: &str,
    version: &str,
) -> Result<ResolvedSoftware, String> {
    let value = parse_json(text, "Paper build metadata")?;
    if value.get("channel").and_then(Value::as_str) != Some("STABLE") {
        return Err(format!(
            "Paper has no stable build for Minecraft {version} yet."
        ));
    }
    let download = value
        .get("downloads")
        .and_then(|downloads| downloads.get("server:default"))
        .ok_or_else(|| {
            format!("The Paper build for {version} has no server download.")
        })?;
    let name = download.get("name").and_then(Value::as_str);
    let url = download.get("url").and_then(Value::as_str);
    let sha256 = download
        .get("checksums")
        .and_then(|sums| sums.get("sha256"))
        .and_then(Value::as_str);
    let (Some(name), Some(url), Some(sha256)) = (name, url, sha256) else {
        return Err(format!("The Paper build for {version} is incomplete."));
    };
    if !valid_checksum(Algorithm::Sha256, sha256) {
        return Err(format!("The Paper checksum for {version} is invalid."));
    }
    if !name.ends_with(".jar")
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        || name.starts_with('.')
    {
        return Err(format!("The Paper file name for {version} is unsafe."));
    }
    Ok(ResolvedSoftware {
        kind: SoftwareKind::Paper,
        version: version.to_string(),
        build: value.get("id").and_then(Value::as_u64),
        file_name: name.to_string(),
        url: url.to_string(),
        size: download.get("size").and_then(Value::as_u64),
        algorithm: Algorithm::Sha256,
        checksum: sha256.to_string(),
        java_major: None,
    })
}

pub fn build_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(concat!("CloudCraft/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if is_allowed_url(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|error| {
            format!("Could not prepare the download client: {error}")
        })
}

/// Streams `url` into `sink`, enforcing the host policy, a size cap, an idle
/// timeout, and cancellation between chunks.
async fn stream_url(
    client: &Client,
    url: &str,
    allow: fn(&Url) -> bool,
    max_bytes: u64,
    tracker: Option<&Tracker>,
    mut sink: impl FnMut(&[u8]) -> Result<(), OpError>,
) -> Result<u64, OpError> {
    let parsed = Url::parse(url)
        .map_err(|error| format!("Invalid download address: {error}"))?;
    let host = parsed.host_str().unwrap_or("unknown host").to_string();
    if !allow(&parsed) {
        return Err(format!(
            "Refusing to download from {host}; only official Mojang and PaperMC hosts over HTTPS are allowed."
        )
        .into());
    }
    if let Some(tracker) = tracker {
        tracker.check_cancel()?;
    }
    let response = client
        .get(parsed)
        .send()
        .await
        .map_err(|error| format!("Could not reach {host}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{host} answered with HTTP {status}.").into());
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(format!(
            "{host} offered a file larger than the allowed limit."
        )
        .into());
    }

    let mut stream = Box::pin(response.bytes_stream());
    let mut total = 0u64;
    let mut last_data = Instant::now();
    loop {
        if let Some(tracker) = tracker {
            tracker.check_cancel()?;
        }
        match tokio::time::timeout(Duration::from_millis(250), stream.next())
            .await
        {
            Err(_) => {
                if last_data.elapsed() > IDLE_LIMIT {
                    return Err(format!(
                        "The download from {host} stalled for {} seconds.",
                        IDLE_LIMIT.as_secs()
                    )
                    .into());
                }
            }
            Ok(None) => break,
            Ok(Some(Err(error))) => {
                return Err(
                    format!("The download was interrupted: {error}").into()
                )
            }
            Ok(Some(Ok(chunk))) => {
                last_data = Instant::now();
                total += chunk.len() as u64;
                if total > max_bytes {
                    return Err(format!(
                        "{host} sent more data than the allowed limit."
                    )
                    .into());
                }
                sink(&chunk)?;
                if let Some(tracker) = tracker {
                    tracker.add_bytes(chunk.len() as u64);
                }
            }
        }
    }
    Ok(total)
}

async fn fetch_text(
    client: &Client,
    url: &str,
    verify: Option<(Algorithm, &str)>,
    tracker: Option<&Tracker>,
) -> Result<String, OpError> {
    let mut body = Vec::new();
    let mut hasher = verify.map(|(algorithm, _)| Hasher::new(algorithm));
    stream_url(
        client,
        url,
        is_allowed_url,
        MAX_METADATA_BYTES,
        tracker,
        |chunk| {
            body.extend_from_slice(chunk);
            if let Some(hasher) = hasher.as_mut() {
                hasher.update(chunk);
            }
            Ok(())
        },
    )
    .await?;
    if let (Some(hasher), Some((_, expected))) = (hasher, verify) {
        if hasher.finish() != expected {
            return Err("Mojang's version metadata failed its checksum and was discarded."
                .to_string()
                .into());
        }
    }
    String::from_utf8(body).map_err(|_| {
        OpError::Failed(
            "The server returned text that is not valid UTF-8.".into(),
        )
    })
}

pub async fn list_versions(
    client: &Client,
    kind: SoftwareKind,
) -> Result<Vec<String>, String> {
    let versions = match kind {
        SoftwareKind::Vanilla => {
            let text = fetch_text(client, VANILLA_MANIFEST_URL, None, None)
                .await
                .map_err(String::from)?;
            parse_vanilla_versions(&text)?
        }
        SoftwareKind::Paper => {
            let text = fetch_text(client, PAPER_API, None, None)
                .await
                .map_err(String::from)?;
            parse_paper_versions(&text)?
        }
    };
    if versions.is_empty() {
        return Err("The publisher returned no versions.".into());
    }
    Ok(versions)
}

async fn resolve_vanilla(
    client: &Client,
    version: &str,
    tracker: Option<&Tracker>,
) -> Result<ResolvedSoftware, OpError> {
    let manifest =
        fetch_text(client, VANILLA_MANIFEST_URL, None, tracker).await?;
    let (url, sha1) = parse_vanilla_entry(&manifest, version)?;
    let package =
        fetch_text(client, &url, Some((Algorithm::Sha1, &sha1)), tracker)
            .await?;
    Ok(parse_vanilla_package(&package, version)?)
}

pub async fn resolve(
    client: &Client,
    kind: SoftwareKind,
    version: &str,
    tracker: Option<&Tracker>,
) -> Result<ResolvedSoftware, OpError> {
    if !valid_version_id(version) {
        return Err(format!("'{version}' is not a valid version id.").into());
    }
    match kind {
        SoftwareKind::Vanilla => {
            resolve_vanilla(client, version, tracker).await
        }
        SoftwareKind::Paper => {
            let url = format!("{PAPER_API}/versions/{version}/builds/latest");
            let text = fetch_text(client, &url, None, tracker).await?;
            let mut resolved = parse_paper_build(&text, version)?;
            // Paper does not publish its Java requirement; the matching
            // Minecraft release does. This is informational only.
            resolved.java_major = resolve_vanilla(client, version, tracker)
                .await
                .ok()
                .and_then(|vanilla| vanilla.java_major);
            Ok(resolved)
        }
    }
}

fn hash_file(
    path: &Path,
    algorithm: Algorithm,
) -> Result<(u64, String), String> {
    let mut file = fs::File::open(path).map_err(|error| {
        format!("Could not read {}: {error}", path.display())
    })?;
    let mut hasher = Hasher::new(algorithm);
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            format!("Could not read {}: {error}", path.display())
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((size, hasher.finish()))
}

fn remove_part(part: &Path, original: OpError) -> OpError {
    match fs::remove_file(part) {
        Ok(()) => original,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => original,
        Err(error) => {
            let reason = match original {
                OpError::Cancelled => "The download was cancelled".to_string(),
                OpError::Failed(message) => message,
            };
            OpError::Failed(format!(
                "{reason}, but the incomplete file '{}' could not be removed: {error}. It is safe to delete manually.",
                part.file_name().unwrap_or_default().to_string_lossy()
            ))
        }
    }
}

/// Downloads `resolved` into `server_dir` and verifies it before it becomes
/// visible under its final name.
pub fn download_verified(
    handle: &Handle,
    client: &Client,
    allow: fn(&Url) -> bool,
    resolved: &ResolvedSoftware,
    server_dir: &Path,
    tracker: &Tracker,
) -> Result<DownloadedSoftware, OpError> {
    valid_component(&resolved.file_name, "The server file name")?;
    if !resolved.file_name.ends_with(".jar") {
        return Err("The server file must be a .jar file.".to_string().into());
    }
    let server_dir = real_directory(server_dir, "server directory")?;
    let target: PathBuf = server_dir.join(&resolved.file_name);
    let report = |reused: bool, bytes: u64| DownloadedSoftware {
        path: target.to_string_lossy().into_owned(),
        file_name: resolved.file_name.clone(),
        bytes,
        algorithm: resolved.algorithm,
        checksum: resolved.checksum.clone(),
        reused,
    };

    if let Ok(metadata) = fs::symlink_metadata(&target) {
        if !metadata.is_file() {
            return Err(format!(
                "'{}' already exists and is not a regular file.",
                resolved.file_name
            )
            .into());
        }
        let (size, checksum) = hash_file(&target, resolved.algorithm)?;
        return if checksum == resolved.checksum {
            Ok(report(true, size))
        } else {
            Err(format!(
                "A different file named '{}' already exists. Rename or remove it, then download again.",
                resolved.file_name
            )
            .into())
        };
    }

    let part =
        server_dir.join(format!("{PART_PREFIX}{}.part", resolved.file_name));
    if let Ok(metadata) = fs::symlink_metadata(&part) {
        if metadata.is_file() {
            let _ = fs::remove_file(&part);
        }
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|error| {
            format!("Could not create the download file: {error}")
        })?;

    tracker.phase("Downloading", 1, 2, resolved.size.unwrap_or(0), 1, true);
    tracker.file_started(&resolved.file_name);
    let mut hasher = Hasher::new(resolved.algorithm);
    let mut head: Vec<u8> = Vec::with_capacity(4);
    let streamed = handle.block_on(stream_url(
        client,
        &resolved.url,
        allow,
        MAX_JAR_BYTES,
        Some(tracker),
        |chunk| {
            if head.len() < JAR_SIGNATURE.len() {
                let missing = JAR_SIGNATURE.len() - head.len();
                head.extend_from_slice(&chunk[..chunk.len().min(missing)]);
            }
            hasher.update(chunk);
            output.write_all(chunk).map_err(|error| {
                OpError::Failed(format!(
                    "Could not write the download: {error}"
                ))
            })
        },
    ));
    let total = match streamed {
        Ok(total) => total,
        Err(error) => {
            drop(output);
            return Err(remove_part(&part, error));
        }
    };

    let verification = (|| -> Result<(), OpError> {
        if resolved.size.is_some_and(|expected| expected != total) {
            return Err(format!(
                "The download was incomplete: expected {} bytes but received {total}.",
                resolved.size.unwrap_or(0)
            )
            .into());
        }
        if hasher.finish() != resolved.checksum {
            return Err("The downloaded file failed its integrity check and was discarded."
                .to_string()
                .into());
        }
        if head != JAR_SIGNATURE {
            return Err(
                "The downloaded file is not a JAR archive and was discarded."
                    .to_string()
                    .into(),
            );
        }
        output.sync_all().map_err(|error| {
            OpError::Failed(format!("Could not flush the download: {error}"))
        })?;
        tracker.file_done();
        Ok(())
    })();
    drop(output);
    if let Err(error) = verification {
        return Err(remove_part(&part, error));
    }

    let commit = (|| -> Result<(), OpError> {
        tracker.begin_commit("Installing verified file")?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(OpError::Failed(format!(
                "'{}' appeared during the download; it was not modified.",
                resolved.file_name
            )));
        }
        fs::rename(&part, &target).map_err(|error| {
            OpError::Failed(format!("Could not install the download: {error}"))
        })
    })();
    if let Err(error) = commit {
        return Err(remove_part(&part, error));
    }
    Ok(report(false, total))
}

/// Resolves and downloads one version. The caller holds the operation slot.
pub fn download(
    handle: &Handle,
    server_dir: &str,
    kind: SoftwareKind,
    version: &str,
    tracker: &Tracker,
) -> Result<DownloadedSoftware, OpError> {
    let client = build_client()?;
    tracker.phase("Looking up version", 1, 2, 0, 0, true);
    let resolved =
        handle.block_on(resolve(&client, kind, version, Some(tracker)))?;
    download_verified(
        handle,
        &client,
        is_allowed_url,
        &resolved,
        Path::new(server_dir),
        tracker,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudcraft_server_ops::{OperationKind, Registry};
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Arc;

    const VANILLA_LIST: &str = r#"{"latest":{"release":"1.21.8","snapshot":"x"},"versions":[
      {"id":"25w01a","type":"snapshot","url":"https://piston-meta.mojang.com/a.json","sha1":"1111111111111111111111111111111111111111"},
      {"id":"1.21.8","type":"release","url":"https://piston-meta.mojang.com/v1/packages/61fff1316bd50312624a858bbb5eb18fa45208ed/1.21.8.json","sha1":"61fff1316bd50312624a858bbb5eb18fa45208ed"},
      {"id":"../evil","type":"release","url":"https://piston-meta.mojang.com/b.json","sha1":"2222222222222222222222222222222222222222"},
      {"id":"1.20.4","type":"release","url":"https://piston-meta.mojang.com/c.json","sha1":"bad"}]}"#;

    const VANILLA_PACKAGE: &str = r#"{"downloads":{"server":{"sha1":"33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c","size":62294556,"url":"https://piston-data.mojang.com/v1/objects/33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c/server.jar"}},"javaVersion":{"component":"java-runtime-epsilon","majorVersion":25}}"#;

    const PAPER_PROJECT: &str = r#"{"project":{"id":"paper"},"versions":{"1.21":["1.21.11","1.21.11-rc3","1.21.8","1.21"],"26.1":["26.1.2","26.1.1"],"1.9":["1.9.4"]}}"#;

    const PAPER_BUILD: &str = r#"{"id":60,"channel":"STABLE","downloads":{"server:default":{"name":"paper-1.21.8-60.jar","checksums":{"sha256":"8de7c52c3b02403503d16fac58003f1efef7dd7a0256786843927fa92ee57f1e"},"size":52811717,"url":"https://fill-data.papermc.io/v1/objects/8de7c52c3b02403503d16fac58003f1efef7dd7a0256786843927fa92ee57f1e/paper-1.21.8-60.jar"}}}"#;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cloudcraft-software-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn local_ok(url: &Url) -> bool {
        url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
    }

    fn jar_bytes(length: usize) -> Vec<u8> {
        let mut bytes = vec![0x50, 0x4b, 0x03, 0x04];
        bytes.extend((0..length).map(|index| (index % 251) as u8));
        bytes
    }

    fn sha256_of(bytes: &[u8]) -> String {
        let mut hasher = Hasher::new(Algorithm::Sha256);
        hasher.update(bytes);
        hasher.finish()
    }

    /// Serves `body` once per connection; `chunk_delay` slows the body down.
    fn serve(body: Vec<u8>, chunk_delay: Option<Duration>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let body = body.clone();
                std::thread::spawn(move || {
                    let mut reader =
                        BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok() {
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        line.clear();
                    }
                    let mut stream = stream;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    for chunk in body.chunks(8 * 1024) {
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                        if let Some(delay) = chunk_delay {
                            std::thread::sleep(delay);
                        }
                    }
                });
            }
        });
        format!("http://{address}/server.jar")
    }

    fn resolved_for(url: String, bytes: &[u8]) -> ResolvedSoftware {
        ResolvedSoftware {
            kind: SoftwareKind::Vanilla,
            version: "1.0".into(),
            build: None,
            file_name: "minecraft_server.1.0.jar".into(),
            url,
            size: Some(bytes.len() as u64),
            algorithm: Algorithm::Sha256,
            checksum: sha256_of(bytes),
            java_major: None,
        }
    }

    fn tracker() -> (Arc<Registry>, Tracker) {
        let registry = Registry::new();
        let tracker = registry.begin(OperationKind::Download).unwrap();
        (registry, tracker)
    }

    #[test]
    fn vanilla_list_keeps_valid_releases_in_publisher_order() {
        assert_eq!(
            parse_vanilla_versions(VANILLA_LIST).unwrap(),
            vec!["1.21.8".to_string(), "1.20.4".to_string()]
        );
        assert!(parse_vanilla_versions("{").is_err());
    }

    #[test]
    fn vanilla_entry_requires_a_release_with_a_valid_checksum() {
        let (url, sha1) = parse_vanilla_entry(VANILLA_LIST, "1.21.8").unwrap();
        assert!(url.ends_with("1.21.8.json"));
        assert_eq!(sha1.len(), 40);
        assert!(parse_vanilla_entry(VANILLA_LIST, "25w01a").is_err());
        assert!(parse_vanilla_entry(VANILLA_LIST, "1.20.4").is_err());
        assert!(parse_vanilla_entry(VANILLA_LIST, "9.9").is_err());
    }

    #[test]
    fn vanilla_package_reads_checksum_size_and_java() {
        let resolved =
            parse_vanilla_package(VANILLA_PACKAGE, "1.21.8").unwrap();
        assert_eq!(resolved.algorithm, Algorithm::Sha1);
        assert_eq!(resolved.size, Some(62294556));
        assert_eq!(resolved.java_major, Some(25));
        assert_eq!(resolved.file_name, "minecraft_server.1.21.8.jar");
        assert!(parse_vanilla_package(r#"{"downloads":{}}"#, "1.0").is_err());
        assert!(parse_vanilla_package(
            r#"{"downloads":{"server":{"sha1":"zz","url":"u"}}}"#,
            "1.0"
        )
        .is_err());
    }

    #[test]
    fn paper_versions_are_stable_only_and_sorted_numerically() {
        assert_eq!(
            parse_paper_versions(PAPER_PROJECT).unwrap(),
            vec!["26.1.2", "26.1.1", "1.21.11", "1.21.8", "1.21", "1.9.4"]
        );
    }

    #[test]
    fn paper_build_requires_stable_channel_and_sha256() {
        let resolved = parse_paper_build(PAPER_BUILD, "1.21.8").unwrap();
        assert_eq!(resolved.build, Some(60));
        assert_eq!(resolved.algorithm, Algorithm::Sha256);
        assert_eq!(resolved.file_name, "paper-1.21.8-60.jar");
        let experimental = PAPER_BUILD.replace("STABLE", "ALPHA");
        assert!(parse_paper_build(&experimental, "1.21.8").is_err());
        let unsafe_name =
            PAPER_BUILD.replace("paper-1.21.8-60.jar\"", "..\\\\evil.jar\"");
        assert!(parse_paper_build(&unsafe_name, "1.21.8").is_err());
    }

    #[test]
    fn only_official_https_hosts_are_allowed() {
        let ok = |value: &str| is_allowed_url(&Url::parse(value).unwrap());
        assert!(ok("https://piston-data.mojang.com/v1/objects/a/server.jar"));
        assert!(ok("https://fill-data.papermc.io/v1/objects/a/p.jar"));
        assert!(!ok("http://piston-data.mojang.com/server.jar"));
        assert!(!ok(
            "https://piston-data.mojang.com.evil.example/server.jar"
        ));
        assert!(!ok("https://evil.example/piston-data.mojang.com"));
        assert!(!ok("https://user:pw@piston-data.mojang.com/server.jar"));
        assert!(!ok("https://piston-data.mojang.com:8443/server.jar"));
        assert!(!ok("https://127.0.0.1/server.jar"));
    }

    #[test]
    fn version_ids_cannot_escape_paths() {
        assert!(valid_version_id("1.21.8"));
        assert!(valid_version_id("1.21-rc1"));
        assert!(!valid_version_id("../etc/passwd"));
        assert!(!valid_version_id(".hidden"));
        assert!(!valid_version_id("a/b"));
        assert!(!valid_version_id(""));
        assert!(!valid_version_id(&"1".repeat(41)));
    }

    #[test]
    fn verified_download_installs_the_file_and_cleans_up() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = temp_dir("ok");
        let bytes = jar_bytes(300_000);
        let resolved = resolved_for(serve(bytes.clone(), None), &bytes);
        let (_registry, tracker) = tracker();
        let client = Client::new();

        let done = download_verified(
            runtime.handle(),
            &client,
            local_ok,
            &resolved,
            &dir,
            &tracker,
        )
        .unwrap();
        assert!(!done.reused);
        assert_eq!(done.bytes, bytes.len() as u64);
        assert_eq!(fs::read(dir.join(&resolved.file_name)).unwrap(), bytes);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.file_name().to_string_lossy().ends_with(".part")
            })
            .collect();
        assert!(leftovers.is_empty());
        drop(tracker);

        let (_registry, tracker) = tracker_pair();
        let again = download_verified(
            runtime.handle(),
            &client,
            local_ok,
            &resolved,
            &dir,
            &tracker,
        )
        .unwrap();
        assert!(again.reused);
        fs::remove_dir_all(dir).unwrap();
    }

    fn tracker_pair() -> (Arc<Registry>, Tracker) {
        tracker()
    }

    #[test]
    fn checksum_mismatch_discards_the_download() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = temp_dir("badsum");
        let bytes = jar_bytes(50_000);
        let mut resolved = resolved_for(serve(bytes.clone(), None), &bytes);
        resolved.checksum = sha256_of(b"something else");
        let (_registry, tracker) = tracker();

        let error = download_verified(
            runtime.handle(),
            &Client::new(),
            local_ok,
            &resolved,
            &dir,
            &tracker,
        )
        .unwrap_err();
        assert!(String::from(error).contains("integrity check"));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn truncated_or_non_jar_downloads_are_rejected() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = temp_dir("shape");
        let bytes = jar_bytes(10_000);

        let mut short = resolved_for(serve(bytes.clone(), None), &bytes);
        short.size = Some(bytes.len() as u64 + 1);
        let (_registry, tracker) = tracker();
        let error = download_verified(
            runtime.handle(),
            &Client::new(),
            local_ok,
            &short,
            &dir,
            &tracker,
        )
        .unwrap_err();
        assert!(String::from(error).contains("incomplete"));
        drop(tracker);

        let html = b"<html>not a jar</html>".to_vec();
        let fake = resolved_for(serve(html.clone(), None), &html);
        let (_registry, tracker) = tracker_pair();
        let error = download_verified(
            runtime.handle(),
            &Client::new(),
            local_ok,
            &fake,
            &dir,
            &tracker,
        )
        .unwrap_err();
        assert!(String::from(error).contains("not a JAR"));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn untrusted_hosts_are_refused_before_any_request() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = temp_dir("host");
        let bytes = jar_bytes(1_000);
        let resolved = resolved_for(serve(bytes.clone(), None), &bytes);
        let (_registry, tracker) = tracker();
        let error = download_verified(
            runtime.handle(),
            &Client::new(),
            is_allowed_url,
            &resolved,
            &dir,
            &tracker,
        )
        .unwrap_err();
        assert!(String::from(error).contains("Refusing to download"));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn existing_different_file_is_never_overwritten() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = temp_dir("exists");
        let bytes = jar_bytes(1_000);
        let resolved = resolved_for(serve(bytes.clone(), None), &bytes);
        fs::write(dir.join(&resolved.file_name), b"mine").unwrap();
        let (_registry, tracker) = tracker();
        let error = download_verified(
            runtime.handle(),
            &Client::new(),
            local_ok,
            &resolved,
            &dir,
            &tracker,
        )
        .unwrap_err();
        assert!(String::from(error).contains("already exists"));
        assert_eq!(fs::read(dir.join(&resolved.file_name)).unwrap(), b"mine");
        fs::remove_dir_all(dir).unwrap();
    }

    /// Talks to the real publishers. Run manually with
    /// `cargo test --package theseus_gui live_publishers -- --ignored`.
    #[test]
    #[ignore = "requires internet access"]
    fn live_publishers_list_resolve_and_download() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = build_client().unwrap();
        for kind in [SoftwareKind::Vanilla, SoftwareKind::Paper] {
            let versions =
                runtime.block_on(list_versions(&client, kind)).unwrap();
            assert!(!versions.is_empty());
            let resolved = runtime
                .block_on(resolve(&client, kind, &versions[0], None))
                .unwrap();
            assert!(is_allowed_url(&Url::parse(&resolved.url).unwrap()));
            println!("{kind:?} latest {} -> {resolved:?}", versions[0]);
        }
        let dir = temp_dir("live");
        let (_registry, tracker) = tracker();
        let done = download(
            runtime.handle(),
            dir.to_str().unwrap(),
            SoftwareKind::Paper,
            "1.21.8",
            &tracker,
        )
        .unwrap();
        println!("downloaded {done:?}");
        assert!(done.bytes > 1_000_000);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancelling_mid_download_removes_partial_data() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = temp_dir("cancel");
        let bytes = jar_bytes(400_000);
        let resolved = resolved_for(
            serve(bytes.clone(), Some(Duration::from_millis(60))),
            &bytes,
        );
        let (registry, tracker) = tracker();
        let canceller = std::thread::spawn({
            let registry = registry.clone();
            move || {
                for _ in 0..100 {
                    std::thread::sleep(Duration::from_millis(50));
                    if registry.request_cancel().is_ok() {
                        return true;
                    }
                }
                false
            }
        });

        let result = download_verified(
            runtime.handle(),
            &Client::new(),
            local_ok,
            &resolved,
            &dir,
            &tracker,
        );
        assert!(canceller.join().unwrap(), "cancel was never accepted");
        assert_eq!(result.unwrap_err(), OpError::Cancelled);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
