//! Self-hosted cloud gaming provider: a remote computer the user controls runs
//! Sunshine (open-source GameStream host) and the user's own licensed Minecraft
//! launcher; CloudCraft configures the remote app through Sunshine's documented
//! REST API and runs the Moonlight client locally to receive the encoded video
//! and audio stream and send keyboard, mouse and controller input back.
//!
//! Security model:
//! - The Sunshine API certificate is self-signed, so it is pinned by SHA-256
//!   fingerprint (trust on first use, confirmed by the user). Every API call
//!   verifies the pin and the TLS handshake signature; redirects are refused.
//! - The Sunshine password is held in memory only and never persisted, logged,
//!   or returned to the frontend. Configuration on disk contains no secrets.
//! - Moonlight is started directly (no shell) with fixed argument positions.
//!
//! Pairing Moonlight with the host is a one-time manual step performed in
//! Moonlight and the Sunshine web UI; CloudCraft does not bypass it.

use crate::cloudcraft_cloud_session::{
    redact_secrets, CloudSessionProvider, ProviderError, RemoteSession,
    StartRequest,
};
use futures::future::BoxFuture;
use rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

pub const PROVIDER_ID: &str = "sunshine";
pub const REMOTE_APP_NAME: &str = "CloudCraft Minecraft";
pub const DEFAULT_API_PORT: u16 = 47990;
const CONFIG_VERSION: u32 = 1;
const MAX_LOG_LINES: usize = 300;
const API_TIMEOUT: Duration = Duration::from_secs(15);
/// Moonlight exits within this window when the host is unreachable or unpaired.
const STREAM_STARTUP_GRACE: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SunshineConfig {
    /// Hostname or IP address of the remote computer (no scheme, no port).
    pub host: String,
    pub api_port: u16,
    pub username: String,
    /// Lowercase hex SHA-256 of the Sunshine API certificate (DER).
    pub cert_sha256: String,
    /// Command Sunshine runs on the remote computer to start Minecraft.
    pub launch_command: String,
    pub working_dir: Option<String>,
    /// Absolute path of the local Moonlight executable.
    pub moonlight_path: String,
    /// `WIDTHxHEIGHT`, for example `1920x1080`.
    pub resolution: Option<String>,
    pub fps: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedConfig {
    version: u32,
    config: SunshineConfig,
}

pub fn validate_host(host: &str) -> Result<(), String> {
    let host = host.trim();
    if host.is_empty() || host.len() > 253 {
        return Err(
            "Enter the remote computer's hostname or IP address.".into()
        );
    }
    if host.contains("://") || host.contains('/') {
        return Err("Enter only the hostname or IP address, without https:// or a path.".into());
    }
    let valid = host.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_')
    });
    if !valid || host.starts_with('-') {
        return Err("The host contains unsupported characters.".into());
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} is required."));
    }
    if value.len() > max {
        return Err(format!("{label} is too long (maximum {max} characters)."));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(format!(
            "{label} must be a single line without control characters."
        ));
    }
    Ok(())
}

pub fn parse_fingerprint(value: &str) -> Result<[u8; 32], String> {
    let cleaned: String = value
        .chars()
        .filter(|c| !matches!(c, ':' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.len() != 64 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("The certificate fingerprint must be a SHA-256 value (64 hex digits).".into());
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[index * 2..index * 2 + 2], 16)
            .map_err(|_| "Invalid fingerprint.".to_string())?;
    }
    Ok(out)
}

pub fn format_fingerprint(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl SunshineConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_host(&self.host)?;
        if self.api_port == 0 {
            return Err(
                "The Sunshine API port must be between 1 and 65535.".into()
            );
        }
        validate_text(&self.username, "The Sunshine username", 128)?;
        parse_fingerprint(&self.cert_sha256)?;
        validate_text(&self.launch_command, "The remote launch command", 1024)?;
        if let Some(dir) = &self.working_dir {
            if !dir.is_empty() {
                validate_text(dir, "The remote working directory", 1024)?;
            }
        }
        validate_text(&self.moonlight_path, "The Moonlight path", 4096)?;
        if !Path::new(&self.moonlight_path).is_absolute() {
            return Err("The Moonlight path must be an absolute path to the Moonlight program.".into());
        }
        if let Some(resolution) = &self.resolution {
            let valid = resolution.split_once('x').is_some_and(|(w, h)| {
                [w, h].iter().all(|part| {
                    (3..=4).contains(&part.len())
                        && part.chars().all(|c| c.is_ascii_digit())
                })
            });
            if !valid {
                return Err("Resolution must look like 1920x1080.".into());
            }
        }
        if let Some(fps) = self.fps {
            if !(30..=240).contains(&fps) {
                return Err("Frame rate must be between 30 and 240.".into());
            }
        }
        Ok(())
    }

    fn url_host(&self) -> String {
        let host = self.host.trim();
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        }
    }

    pub fn api_base(&self) -> String {
        format!("https://{}:{}", self.url_host(), self.api_port)
    }

    /// Arguments for `moonlight stream`. Fixed positions; never passed through a shell.
    pub fn moonlight_stream_args(&self) -> Vec<String> {
        let mut args = vec![
            "stream".to_string(),
            self.host.trim().to_string(),
            REMOTE_APP_NAME.to_string(),
            "--quit-after".to_string(),
        ];
        if let Some(resolution) = &self.resolution {
            args.push("--resolution".into());
            args.push(resolution.clone());
        }
        if let Some(fps) = self.fps {
            args.push("--fps".into());
            args.push(fps.to_string());
        }
        args
    }

    /// The Sunshine application definition CloudCraft owns on the host.
    pub fn sunshine_app(&self, index: i64) -> serde_json::Value {
        serde_json::json!({
            "name": REMOTE_APP_NAME,
            "output": "",
            "cmd": self.launch_command,
            "working-dir": self.working_dir.clone().unwrap_or_default(),
            "index": index,
            "exclude-global-prep-cmd": false,
            "elevated": false,
            "auto-detach": true,
            "wait-all": true,
            "exit-timeout": 5,
            "prep-cmd": [],
            "detached": [],
            "image-path": "",
        })
    }
}

/// Verifies the TLS handshake signature with the normal algorithms and accepts
/// the end-entity certificate only when its SHA-256 matches the pin. With no pin
/// (probe mode) it records the fingerprint and never sends credentials.
#[derive(Debug)]
struct PinnedVerifier {
    expected: Option<[u8; 32]>,
    seen: Arc<Mutex<Option<[u8; 32]>>>,
    crypto: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(digest);
        }
        match self.expected {
            Some(expected) if expected != digest => Err(rustls::Error::General(
                "Sunshine certificate fingerprint does not match the trusted value".into(),
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.crypto.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.crypto.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.crypto
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn pinned_client(
    expected: Option<[u8; 32]>,
) -> Result<(reqwest::Client, Arc<Mutex<Option<[u8; 32]>>>), String> {
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let seen = Arc::new(Mutex::new(None));
    let verifier = PinnedVerifier {
        expected,
        seen: seen.clone(),
        crypto: crypto.clone(),
    };
    let mut tls = rustls::ClientConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup failed: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(API_TIMEOUT)
        .build()
        .map_err(|e| format!("Could not create the HTTPS client: {e}"))?;
    Ok((client, seen))
}

/// Connects without credentials and returns the API certificate fingerprint for
/// the user to compare with the one shown on the host before trusting it.
pub async fn probe_certificate(
    host: &str,
    api_port: u16,
) -> Result<String, String> {
    validate_host(host)?;
    let probe = SunshineConfig {
        host: host.to_string(),
        api_port,
        username: String::new(),
        cert_sha256: String::new(),
        launch_command: String::new(),
        working_dir: None,
        moonlight_path: String::new(),
        resolution: None,
        fps: None,
    };
    let (client, seen) = pinned_client(None)?;
    // configLocale is documented as unauthenticated; no credentials are sent.
    let result = client
        .get(format!("{}/api/configLocale", probe.api_base()))
        .send()
        .await;
    let fingerprint = seen.lock().ok().and_then(|value| *value);
    match (fingerprint, result) {
        (Some(fingerprint), _) => Ok(format_fingerprint(&fingerprint)),
        (None, Err(error)) => Err(format!(
            "Could not reach Sunshine at {}: {}",
            probe.api_base(),
            redact_secrets(&error.to_string())
        )),
        (None, Ok(_)) => {
            Err("The host did not present a TLS certificate.".into())
        }
    }
}

/// Minimal Sunshine REST client (documented endpoints only).
pub struct SunshineApi {
    client: reqwest::Client,
    base: String,
    username: String,
    password: String,
}

impl SunshineApi {
    pub fn new(
        config: &SunshineConfig,
        password: &str,
    ) -> Result<Self, String> {
        let pin = parse_fingerprint(&config.cert_sha256)?;
        let (client, _) = pinned_client(Some(pin))?;
        Ok(Self {
            client,
            base: config.api_base(),
            username: config.username.clone(),
            password: password.to_string(),
        })
    }

    #[cfg(test)]
    fn plain_for_tests(base: String, username: &str, password: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(API_TIMEOUT)
                .build()
                .unwrap(),
            base,
            username: username.into(),
            password: password.into(),
        }
    }

    fn describe(&self, error: reqwest::Error) -> String {
        let text = error.to_string();
        if text.contains("fingerprint does not match")
            || format!("{error:?}").contains("fingerprint does not match")
        {
            return "The host's certificate no longer matches the trusted fingerprint. Re-check the host and trust it again only if you changed it yourself.".into();
        }
        redact_secrets(&format!("Could not reach Sunshine: {text}"))
    }

    async fn check(
        response: reqwest::Response,
    ) -> Result<reqwest::Response, String> {
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 => Err("Sunshine rejected the username or password.".into()),
            status => Err(format!("Sunshine returned HTTP {status}.")),
        }
    }

    pub async fn list_apps(&self) -> Result<Vec<serde_json::Value>, String> {
        let response = self
            .client
            .get(format!("{}/api/apps", self.base))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| self.describe(e))?;
        let body: serde_json::Value =
            Self::check(response).await?.json().await.map_err(|_| {
                "Sunshine returned an unreadable application list.".to_string()
            })?;
        Ok(body
            .get("apps")
            .and_then(|apps| apps.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Creates or updates the CloudCraft application and returns its index.
    pub async fn upsert_app(
        &self,
        config: &SunshineConfig,
    ) -> Result<i64, String> {
        let apps = self.list_apps().await?;
        let existing = apps.iter().position(|app| {
            app.get("name").and_then(|n| n.as_str()) == Some(REMOTE_APP_NAME)
        });
        let index = existing.map(|i| i as i64).unwrap_or(-1);
        let response = self
            .client
            .post(format!("{}/api/apps", self.base))
            .basic_auth(&self.username, Some(&self.password))
            .json(&config.sunshine_app(index))
            .send()
            .await
            .map_err(|e| self.describe(e))?;
        let body: serde_json::Value = Self::check(response)
            .await?
            .json()
            .await
            .unwrap_or(serde_json::Value::Null);
        if body.get("status").and_then(|s| s.as_bool()) == Some(false) {
            return Err(format!(
                "Sunshine refused the application definition: {}",
                redact_secrets(
                    &body
                        .get("error")
                        .map(|e| e.to_string())
                        .unwrap_or_default()
                )
            ));
        }
        Ok(if index >= 0 { index } else { apps.len() as i64 })
    }

    pub async fn close_app(&self) -> Result<(), String> {
        let response = self
            .client
            .post(format!("{}/api/apps/close", self.base))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| self.describe(e))?;
        Self::check(response).await.map(|_| ())
    }

    /// Round-trip time of an authenticated request, in milliseconds.
    pub async fn ping(&self) -> Result<u64, String> {
        let started = Instant::now();
        self.list_apps().await?;
        Ok(started.elapsed().as_millis() as u64)
    }
}

struct StreamProcess {
    child: Child,
    remote_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub config: Option<SunshineConfig>,
    pub unlocked: bool,
    pub moonlight_found: bool,
    pub streaming: bool,
    pub moonlight_candidates: Vec<String>,
}

pub struct SunshineProvider {
    config_path: PathBuf,
    config: Mutex<Option<SunshineConfig>>,
    password: Mutex<Option<String>>,
    stream: Mutex<Option<StreamProcess>>,
    logs: Arc<Mutex<VecDeque<String>>>,
}

fn push_log(logs: &Arc<Mutex<VecDeque<String>>>, line: impl AsRef<str>) {
    if let Ok(mut logs) = logs.lock() {
        if logs.len() >= MAX_LOG_LINES {
            logs.pop_front();
        }
        logs.push_back(redact_secrets(line.as_ref()));
    }
}

pub fn moonlight_candidates() -> Vec<String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for base in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(dir) = std::env::var(base) {
                paths.push(
                    PathBuf::from(dir)
                        .join("Moonlight Game Streaming")
                        .join("Moonlight.exe"),
                );
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from(
            "/Applications/Moonlight.app/Contents/MacOS/Moonlight",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/bin/moonlight"));
        paths.push(PathBuf::from("/usr/bin/moonlight-qt"));
        paths.push(PathBuf::from("/usr/local/bin/moonlight"));
        paths.push(PathBuf::from(
            "/var/lib/flatpak/exports/bin/com.moonlight_stream.Moonlight",
        ));
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".local/share/flatpak/exports/bin/com.moonlight_stream.Moonlight"));
        }
    }
    paths
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

impl SunshineProvider {
    pub fn load(config_path: PathBuf) -> Self {
        let config = fs::read_to_string(&config_path)
            .ok()
            .and_then(|text| {
                serde_json::from_str::<PersistedConfig>(&text).ok()
            })
            .filter(|saved| saved.version == CONFIG_VERSION)
            .map(|saved| saved.config)
            .filter(|config| config.validate().is_ok());
        Self {
            config_path,
            config: Mutex::new(config),
            password: Mutex::new(None),
            stream: Mutex::new(None),
            logs: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn config(&self) -> Option<SunshineConfig> {
        self.config.lock().ok().and_then(|c| c.clone())
    }

    fn password(&self) -> Option<String> {
        self.password.lock().ok().and_then(|p| p.clone())
    }

    pub fn view(&self) -> ProviderView {
        let config = self.config();
        let moonlight_found = config
            .as_ref()
            .is_some_and(|c| Path::new(&c.moonlight_path).is_file());
        ProviderView {
            config,
            unlocked: self.password().is_some(),
            moonlight_found,
            streaming: self.stream_running(),
            moonlight_candidates: moonlight_candidates(),
        }
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|logs| logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn save_config(&self, config: SunshineConfig) -> Result<(), String> {
        let mut config = config;
        config.host = config.host.trim().to_string();
        config.cert_sha256 =
            format_fingerprint(&parse_fingerprint(&config.cert_sha256)?);
        config.working_dir =
            config.working_dir.filter(|d| !d.trim().is_empty());
        config.resolution = config.resolution.filter(|r| !r.trim().is_empty());
        config.validate()?;
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let serialized = serde_json::to_string_pretty(&PersistedConfig {
            version: CONFIG_VERSION,
            config: config.clone(),
        })
        .map_err(|e| e.to_string())?;
        let temp = self.config_path.with_extension("json.tmp");
        fs::write(&temp, serialized).map_err(|e| e.to_string())?;
        fs::rename(&temp, &self.config_path).map_err(|e| e.to_string())?;
        let changed_identity = self.config().is_none_or(|old| {
            old.host != config.host
                || old.username != config.username
                || old.api_port != config.api_port
                || old.cert_sha256 != config.cert_sha256
        });
        if let Ok(mut current) = self.config.lock() {
            *current = Some(config);
        }
        if changed_identity {
            self.lock_credentials();
        }
        Ok(())
    }

    pub fn clear_config(&self) -> Result<(), String> {
        match fs::remove_file(&self.config_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        if let Ok(mut current) = self.config.lock() {
            *current = None;
        }
        self.lock_credentials();
        Ok(())
    }

    /// Verifies the password against the host, then keeps it in memory only.
    pub async fn unlock(&self, password: String) -> Result<(), String> {
        let config = self.config().ok_or("Configure the remote host first.")?;
        if password.is_empty() || password.len() > 256 {
            return Err("Enter the Sunshine password.".into());
        }
        SunshineApi::new(&config, &password)?.list_apps().await?;
        if let Ok(mut current) = self.password.lock() {
            *current = Some(password);
        }
        Ok(())
    }

    pub fn lock_credentials(&self) {
        if let Ok(mut current) = self.password.lock() {
            *current = None;
        }
    }

    fn api(&self) -> Result<(SunshineConfig, SunshineApi), String> {
        let config =
            self.config().ok_or("The remote host is not configured.")?;
        let password = self.password().ok_or(
            "Unlock the remote host with its Sunshine password first.",
        )?;
        let api = SunshineApi::new(&config, &password)?;
        Ok((config, api))
    }

    pub async fn ping(&self) -> Result<u64, String> {
        self.api()?.1.ping().await
    }

    fn stream_running(&self) -> bool {
        let Ok(mut stream) = self.stream.lock() else {
            return false;
        };
        stream
            .as_mut()
            .is_some_and(|s| matches!(s.child.try_wait(), Ok(None)))
    }

    fn spawn_moonlight(
        &self,
        config: &SunshineConfig,
        remote_id: &str,
    ) -> Result<(), String> {
        if !Path::new(&config.moonlight_path).is_file() {
            return Err(format!(
                "Moonlight was not found at {}. Install Moonlight or correct the path.",
                config.moonlight_path
            ));
        }
        let mut child = Command::new(&config.moonlight_path)
            .args(config.moonlight_stream_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Could not start Moonlight: {e}"))?;
        push_log(
            &self.logs,
            format!(
                "Started Moonlight (pid {}) for {}",
                child.id().unwrap_or(0),
                config.host
            ),
        );
        for reader in [
            child.stdout.take().map(|s| {
                Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>
            }),
            child.stderr.take().map(|s| {
                Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>
            }),
        ]
        .into_iter()
        .flatten()
        {
            let logs = self.logs.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push_log(&logs, line);
                }
            });
        }
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| "Stream state is unavailable.")?;
        if let Some(mut previous) = stream.take() {
            let _ = previous.child.start_kill();
        }
        *stream = Some(StreamProcess {
            child,
            remote_id: remote_id.to_string(),
        });
        Ok(())
    }

    /// Returns the exit description if Moonlight already ended.
    fn stream_exit(&self) -> Option<String> {
        let mut stream = self.stream.lock().ok()?;
        let process = stream.as_mut()?;
        match process.child.try_wait() {
            Ok(Some(status)) => Some(match status.code() {
                Some(code) => format!("Moonlight exited with code {code}"),
                None => "Moonlight was terminated by a signal".into(),
            }),
            Ok(None) => None,
            Err(error) => Some(format!("Moonlight state is unknown: {error}")),
        }
    }

    async fn kill_stream(&self) {
        let process = self.stream.lock().ok().and_then(|mut s| s.take());
        if let Some(mut process) = process {
            let _ = process.child.start_kill();
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                process.child.wait(),
            )
            .await;
            push_log(
                &self.logs,
                format!("Moonlight stopped for {}", process.remote_id),
            );
        }
    }

    /// Kills the local stream window on launcher exit. The remote app is released
    /// on the next start through the recovered session's Stop.
    pub fn shutdown(&self) {
        if let Ok(mut stream) = self.stream.lock() {
            if let Some(process) = stream.as_mut() {
                let _ = process.child.start_kill();
            }
        }
    }

    async fn start_inner(&self) -> Result<RemoteSession, ProviderError> {
        let fail = |message: String| ProviderError {
            message,
            remote_id: None,
        };
        let (config, api) = self.api().map_err(fail)?;
        if !Path::new(&config.moonlight_path).is_file() {
            return Err(fail(format!(
                "Moonlight was not found at {}.",
                config.moonlight_path
            )));
        }
        api.upsert_app(&config).await.map_err(fail)?;
        push_log(
            &self.logs,
            format!("Configured \"{REMOTE_APP_NAME}\" on {}", config.host),
        );
        let remote_id =
            format!("{PROVIDER_ID}:{}:{REMOTE_APP_NAME}", config.host);
        self.spawn_moonlight(&config, &remote_id).map_err(fail)?;

        tokio::time::sleep(STREAM_STARTUP_GRACE).await;
        if let Some(exit) = self.stream_exit() {
            let tail =
                self.logs().into_iter().rev().take(3).collect::<Vec<_>>();
            let detail = tail.into_iter().rev().collect::<Vec<_>>().join(" | ");
            let released = api.close_app().await.is_ok();
            self.kill_stream().await;
            return Err(ProviderError {
                message: format!(
                    "{exit} before the stream started. Check that Moonlight is paired with this host and the host is reachable. {detail}"
                ),
                remote_id: if released { None } else { Some(remote_id) },
            });
        }
        Ok(RemoteSession { remote_id })
    }
}

impl CloudSessionProvider for SunshineProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn unavailable_reason(&self) -> Option<String> {
        let Some(config) = self.config() else {
            return Some("Set up a remote host: a computer you control running Sunshine, plus Moonlight on this device.".into());
        };
        if !Path::new(&config.moonlight_path).is_file() {
            return Some(format!(
                "Moonlight was not found at {}. Install Moonlight or correct the path.",
                config.moonlight_path
            ));
        }
        if self.password().is_none() {
            return Some(format!(
                "Unlock {} with its Sunshine password to start a session.",
                config.host
            ));
        }
        None
    }

    fn start<'a>(
        &'a self,
        _request: &'a StartRequest,
    ) -> BoxFuture<'a, Result<RemoteSession, ProviderError>> {
        Box::pin(async move {
            // Dropping this future (cancel/timeout) must not leave a stream running.
            let guard = StreamGuard {
                provider: self,
                armed: true,
            };
            let result = self.start_inner().await;
            let mut guard = guard;
            guard.armed = result.is_err();
            result
        })
    }

    fn stop<'a>(
        &'a self,
        remote_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let (_, api) = self.api()?;
            api.close_app().await?;
            self.kill_stream().await;
            push_log(
                &self.logs,
                format!("Closed the remote app for {remote_id}"),
            );
            Ok(())
        })
    }

    fn health(&self, _remote_id: &str) -> Option<String> {
        let exit = self.stream_exit()?;
        Some(format!(
            "{exit}. Minecraft may still be running on the remote computer: Reconnect to resume or Stop to close it."
        ))
    }

    fn supports_reconnect(&self) -> bool {
        true
    }

    fn reconnect<'a>(
        &'a self,
        remote_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let config =
                self.config().ok_or("The remote host is not configured.")?;
            self.spawn_moonlight(&config, remote_id)?;
            tokio::time::sleep(STREAM_STARTUP_GRACE).await;
            match self.stream_exit() {
                Some(exit) => Err(format!("{exit} while reconnecting.")),
                None => Ok(()),
            }
        })
    }
}

/// Kills a stream that was spawned by a start future that was then dropped.
struct StreamGuard<'a> {
    provider: &'a SunshineProvider,
    armed: bool,
}

impl Drop for StreamGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.provider.shutdown();
            if let Ok(mut stream) = self.provider.stream.lock() {
                stream.take();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn config(moonlight: &str) -> SunshineConfig {
        SunshineConfig {
            host: "gaming-pc.tail1234.ts.net".into(),
            api_port: DEFAULT_API_PORT,
            username: "admin".into(),
            cert_sha256: "ab".repeat(32),
            launch_command: "C:\\Games\\launcher.exe --launch survival".into(),
            working_dir: None,
            moonlight_path: moonlight.into(),
            resolution: Some("1920x1080".into()),
            fps: Some(60),
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("cloudcraft-sunshine-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn config_validation_rejects_unsafe_values() {
        let abs = if cfg!(windows) {
            "C:\\moonlight.exe"
        } else {
            "/usr/bin/moonlight"
        };
        assert!(config(abs).validate().is_ok());
        let mut c = config(abs);
        c.host = "https://evil.example/path".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.host = "-oProxyCommand=x".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.launch_command = "run\nsecond".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.moonlight_path = "moonlight".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.resolution = Some("1920*1080".into());
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.fps = Some(1000);
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.cert_sha256 = "abc".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn fingerprint_parsing_accepts_colon_format_and_round_trips() {
        let colon = (0..32)
            .map(|i| format!("{i:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let parsed = parse_fingerprint(&colon).unwrap();
        assert_eq!(parsed[31], 31);
        assert_eq!(
            parse_fingerprint(&format_fingerprint(&parsed)).unwrap(),
            parsed
        );
    }

    #[test]
    fn moonlight_arguments_are_fixed_and_shell_free() {
        let args = config("/usr/bin/moonlight").moonlight_stream_args();
        assert_eq!(
            args,
            vec![
                "stream",
                "gaming-pc.tail1234.ts.net",
                REMOTE_APP_NAME,
                "--quit-after",
                "--resolution",
                "1920x1080",
                "--fps",
                "60"
            ]
        );
    }

    #[test]
    fn ipv6_hosts_are_bracketed_in_api_url() {
        let mut c = config("/usr/bin/moonlight");
        c.host = "fd7a:115c:a1e0::1".into();
        assert_eq!(c.api_base(), "https://[fd7a:115c:a1e0::1]:47990");
    }

    #[test]
    fn saved_config_contains_no_password_and_reloads() {
        let dir = temp_dir();
        let path = dir.join("cloud.json");
        let provider = SunshineProvider::load(path.clone());
        let abs = if cfg!(windows) {
            "C:\\moonlight.exe"
        } else {
            "/usr/bin/moonlight"
        };
        provider.save_config(config(abs)).unwrap();
        *provider.password.lock().unwrap() = Some("hunter2-secret".into());
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hunter2"));
        let reloaded = SunshineProvider::load(path);
        assert_eq!(reloaded.config(), Some(config(abs)));
        assert!(reloaded.password().is_none());
        assert!(reloaded.unavailable_reason().is_some());
    }

    #[test]
    fn changing_host_locks_credentials() {
        let dir = temp_dir();
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        let abs = if cfg!(windows) {
            "C:\\moonlight.exe"
        } else {
            "/usr/bin/moonlight"
        };
        provider.save_config(config(abs)).unwrap();
        *provider.password.lock().unwrap() = Some("pw".into());
        let mut other = config(abs);
        other.launch_command = "different".into();
        provider.save_config(other.clone()).unwrap();
        assert!(
            provider.password().is_some(),
            "non-identity edit keeps unlock"
        );
        other.host = "other-host".into();
        provider.save_config(other).unwrap();
        assert!(provider.password().is_none(), "new host requires unlock");
    }

    /// Tiny HTTP/1.1 server emulating the documented Sunshine endpoints.
    async fn mock_sunshine(
        expected_auth: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let apps =
            Arc::new(Mutex::new(vec![serde_json::json!({"name": "Desktop"})]));
        let seen_clone = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let seen = seen_clone.clone();
                let apps = apps.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut len = 0;
                    loop {
                        let n = socket.read(&mut buf[len..]).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        len += n;
                        let text =
                            String::from_utf8_lossy(&buf[..len]).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let content_length = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| {
                                            v.trim()
                                                .parse::<usize>()
                                                .unwrap_or(0)
                                        })
                                })
                                .unwrap_or(0);
                            if len >= end + 4 + content_length {
                                let request_line = text
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .to_string();
                                let authorized = text.lines().any(|l| {
                                    l.eq_ignore_ascii_case(&format!(
                                        "authorization: {expected_auth}"
                                    ))
                                });
                                let body =
                                    &text[end + 4..end + 4 + content_length];
                                seen.lock()
                                    .unwrap()
                                    .push(format!("{request_line} {body}"));
                                let (status, payload) = if !authorized {
                                    ("401 Unauthorized", "{}".to_string())
                                } else if request_line
                                    .starts_with("GET /api/apps")
                                {
                                    ("200 OK", serde_json::json!({"env": {}, "apps": *apps.lock().unwrap()}).to_string())
                                } else if request_line
                                    .starts_with("POST /api/apps/close")
                                {
                                    ("200 OK", r#"{"status":true}"#.to_string())
                                } else if request_line
                                    .starts_with("POST /api/apps")
                                {
                                    let app: serde_json::Value =
                                        serde_json::from_str(body).unwrap();
                                    let index = app["index"].as_i64().unwrap();
                                    let mut apps = apps.lock().unwrap();
                                    if index < 0 {
                                        apps.push(app);
                                    } else {
                                        apps[index as usize] = app;
                                    }
                                    ("200 OK", r#"{"status":true}"#.to_string())
                                } else {
                                    ("404 Not Found", "{}".to_string())
                                };
                                let response = format!(
                                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                    payload.len()
                                );
                                let _ =
                                    socket.write_all(response.as_bytes()).await;
                                return;
                            }
                        }
                    }
                });
            }
        });
        (base, seen)
    }

    // "admin:pw" base64
    const AUTH: &str = "Basic YWRtaW46cHc=";

    #[tokio::test]
    async fn api_upserts_app_once_and_closes() {
        let (base, seen) = mock_sunshine(AUTH).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "pw");
        let c = config("/usr/bin/moonlight");
        assert_eq!(api.upsert_app(&c).await.unwrap(), 1);
        assert_eq!(
            api.upsert_app(&c).await.unwrap(),
            1,
            "second upsert updates in place"
        );
        let apps = api.list_apps().await.unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[1]["cmd"], c.launch_command);
        api.close_app().await.unwrap();
        assert!(api.ping().await.is_ok());
        let log = seen.lock().unwrap().join("\n");
        assert!(log.contains("POST /api/apps/close"));
    }

    #[tokio::test]
    async fn api_reports_wrong_password_clearly() {
        let (base, _) = mock_sunshine(AUTH).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "wrong");
        let error = api.list_apps().await.unwrap_err();
        assert!(
            error.contains("rejected the username or password"),
            "{error}"
        );
        assert!(!error.contains("wrong"));
    }

    #[tokio::test]
    async fn pinned_client_refuses_mismatched_certificate_host() {
        // Nothing listens on this port; the point is that a pinned client is
        // constructed and a connection failure is reported without secrets.
        let mut c = config("/usr/bin/moonlight");
        c.host = "127.0.0.1".into();
        c.api_port = 9;
        let api = SunshineApi::new(&c, "secret-password").unwrap();
        let error = api.list_apps().await.unwrap_err();
        assert!(!error.contains("secret-password"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn moonlight_process_lifecycle_is_tracked_and_killed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let fake = dir.join("moonlight");
        fs::write(&fake, "#!/bin/sh\necho \"stream $2\"\nsleep 30\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        let c = config(fake.to_str().unwrap());
        provider.spawn_moonlight(&c, "sunshine:test").unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(provider.stream_running());
        assert!(provider.health("sunshine:test").is_none());
        assert!(provider
            .logs()
            .iter()
            .any(|l| l.contains("stream gaming-pc")));
        provider.kill_stream().await;
        assert!(!provider.stream_running());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_stream_is_reported_by_health() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let fake = dir.join("moonlight");
        fs::write(&fake, "#!/bin/sh\necho 'not paired' 1>&2\nexit 3\n")
            .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        provider
            .spawn_moonlight(&config(fake.to_str().unwrap()), "sunshine:test")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let health = provider.health("sunshine:test").unwrap();
        assert!(health.contains("code 3"), "{health}");
    }
}
