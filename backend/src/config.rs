use crate::oauth::{OAuthClient, OAuthProviders, Provider};
use oauth2::{basic::BasicClient, AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tower_cookies::Key;
use url::Url;

/// Default cap on a single upload (and any request body): 50 MiB.
const DEFAULT_MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;
const DEFAULT_SESSION_TTL_DAYS: i64 = 30;

/// Server configuration resolved from the environment.
///
/// Each value falls back to a sensible default and is logged at startup, so the
/// effective configuration is always visible in the logs. Secrets are never
/// logged — only whether they were set.
#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// Externally visible base URL (`PUBLIC_URL`). Browser-originated
    /// WebSocket upgrades must come from this origin.
    pub public_url: Url,
    /// Encrypts and authenticates every cookie the server sets.
    pub cookie_key: Key,
    pub oauth: OAuthProviders,
    /// Lowercase email domains allowed to sign in. Empty means anyone.
    pub allowed_email_domains: Vec<String>,
    pub max_upload_bytes: usize,
    pub session_ttl: chrono::Duration,
}

impl Config {
    /// Read configuration from the environment, applying defaults and logging
    /// each resolved value.
    ///
    /// Outside dev mode a session secret of at least 64 bytes (`SESSION_SECRET`
    /// or the file named by `SESSION_SECRET_FILE`) and at least
    /// one OAuth provider are required; misconfiguration fails at boot rather
    /// than on the first login attempt.
    pub fn from_env(dev_mode: bool) -> anyhow::Result<Self> {
        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000);
        let public_url =
            env::var("PUBLIC_URL").unwrap_or_else(|_| format!("http://localhost:{port}"));
        let public_url = Url::parse(&public_url)
            .map_err(|e| anyhow::anyhow!("PUBLIC_URL {public_url:?} is not a valid URL: {e}"))?;
        let max_upload_bytes = env::var("MAX_UPLOAD_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_UPLOAD_BYTES);
        let session_ttl_days = env::var("SESSION_TTL_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_SESSION_TTL_DAYS);
        let allowed_email_domains =
            parse_domains(&env::var("ALLOWED_EMAIL_DOMAINS").unwrap_or_default());

        tracing::info!("Config: HOST={host}");
        tracing::info!("Config: PORT={port}");
        tracing::info!("Config: PUBLIC_URL={public_url}");
        tracing::info!("Config: MAX_UPLOAD_BYTES={max_upload_bytes}");
        tracing::info!("Config: SESSION_TTL_DAYS={session_ttl_days}");
        if allowed_email_domains.is_empty() {
            tracing::info!("Config: ALLOWED_EMAIL_DOMAINS=<any>");
        } else {
            tracing::info!(
                "Config: ALLOWED_EMAIL_DOMAINS={}",
                allowed_email_domains.join(",")
            );
        }

        let cookie_key = session_key(
            env::var_os("SESSION_SECRET"),
            env::var_os("SESSION_SECRET_FILE").map(PathBuf::from),
            dev_mode,
        )?;

        let oauth = OAuthProviders {
            google: build_provider(Provider::Google)?,
            github: build_provider(Provider::GitHub)?,
        };
        let enabled: Vec<&str> = oauth.enabled().iter().map(|p| p.key()).collect();
        tracing::info!("Config: OAuth providers=[{}]", enabled.join(","));
        if enabled.is_empty() && !dev_mode {
            anyhow::bail!(
                "No OAuth provider is configured, so nobody could sign in. Set \
                 GOOGLE_CLIENT_ID/GOOGLE_CLIENT_SECRET/GOOGLE_REDIRECT_URI or \
                 GITHUB_CLIENT_ID/GITHUB_CLIENT_SECRET/GITHUB_REDIRECT_URI, or \
                 pass --dev-mode."
            );
        }

        Ok(Self {
            host,
            port,
            public_url,
            cookie_key,
            oauth,
            allowed_email_domains,
            max_upload_bytes,
            session_ttl: chrono::Duration::days(session_ttl_days),
        })
    }

    /// The `host:port` address to bind the server to.
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Whether `email` may sign in under `ALLOWED_EMAIL_DOMAINS`.
    pub fn email_allowed(&self, email: &str) -> bool {
        if self.allowed_email_domains.is_empty() {
            return true;
        }
        let Some((_, domain)) = email.rsplit_once('@') else {
            return false;
        };
        self.allowed_email_domains.contains(&domain.to_lowercase())
    }
}

/// Resolve the cookie key from `SESSION_SECRET` or `SESSION_SECRET_FILE`.
///
/// The file holds the same value the variable would; exactly one trailing
/// newline (`\n` or `\r\n`) is stripped, so both sources yield the same key
/// and sessions survive switching between them. Setting both is an error, and
/// so is a file that is missing, unreadable or too short, in every mode:
/// there is never a fallback to a random key once a file is named. Only an
/// entirely unset secret in dev mode gets an ephemeral key. Neither the value
/// nor its length is ever logged or included in an error.
fn session_key(
    secret: Option<OsString>,
    secret_file: Option<PathBuf>,
    dev_mode: bool,
) -> anyhow::Result<Key> {
    match (secret, secret_file) {
        (Some(_), Some(_)) => {
            anyhow::bail!("SESSION_SECRET and SESSION_SECRET_FILE are both set; set only one")
        }
        (Some(secret), None) => {
            tracing::info!("Config: SESSION_SECRET=<set>");
            let secret = secret
                .into_string()
                .map_err(|_| anyhow::anyhow!("SESSION_SECRET must be valid UTF-8"))?;
            key_from_secret(secret.as_bytes(), "SESSION_SECRET")
        }
        (None, Some(path)) => {
            tracing::info!("Config: SESSION_SECRET_FILE={}", path.display());
            let bytes = read_secret_file(&path)?;
            key_from_secret(&bytes, "SESSION_SECRET_FILE")
        }
        (None, None) if dev_mode => {
            tracing::warn!(
                "Config: SESSION_SECRET unset; using a random key (sessions end on restart)"
            );
            Ok(Key::generate())
        }
        (None, None) => {
            anyhow::bail!("SESSION_SECRET or SESSION_SECRET_FILE must be set (at least 64 bytes)")
        }
    }
}

/// Read a secret file, stripping exactly one trailing `\n` or `\r\n`.
fn read_secret_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut bytes = std::fs::read(path).map_err(|e| {
        anyhow::anyhow!(
            "SESSION_SECRET_FILE {} could not be read: {e}",
            path.display()
        )
    })?;
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
    Ok(bytes)
}

fn key_from_secret(secret: &[u8], source: &str) -> anyhow::Result<Key> {
    Key::try_from(secret).map_err(|_| anyhow::anyhow!("{source} must be at least 64 bytes long"))
}

fn parse_domains(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|d| d.trim().trim_start_matches('@').to_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

/// Build one provider's client from `{PREFIX}_CLIENT_ID` / `_CLIENT_SECRET` /
/// `_REDIRECT_URI`. All unset (or empty) means the provider is disabled; a
/// partial set is an error naming the missing variables.
fn build_provider(provider: Provider) -> anyhow::Result<Option<OAuthClient>> {
    let prefix = provider.env_prefix();
    let names = [
        format!("{prefix}_CLIENT_ID"),
        format!("{prefix}_CLIENT_SECRET"),
        format!("{prefix}_REDIRECT_URI"),
    ];
    let values = names
        .clone()
        .map(|n| env::var(n).ok().filter(|v| !v.is_empty()));
    match values {
        [None, None, None] => Ok(None),
        [Some(id), Some(secret), Some(redirect)] => {
            Ok(Some(oauth_client(provider, id, secret, redirect)?))
        }
        values => {
            let missing: Vec<&str> = names
                .iter()
                .zip(&values)
                .filter(|(_, v)| v.is_none())
                .map(|(n, _)| n.as_str())
                .collect();
            anyhow::bail!(
                "{prefix} login is partially configured; missing {}",
                missing.join(", ")
            )
        }
    }
}

/// Construct the OAuth client for `provider` with the given credentials.
pub fn oauth_client(
    provider: Provider,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
) -> anyhow::Result<OAuthClient> {
    Ok(BasicClient::new(ClientId::new(client_id))
        .set_client_secret(ClientSecret::new(client_secret))
        .set_auth_uri(AuthUrl::new(provider.auth_url().to_string())?)
        .set_token_uri(TokenUrl::new(provider.token_url().to_string())?)
        .set_redirect_uri(RedirectUrl::new(redirect_uri)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_domains_normalizes() {
        assert_eq!(
            parse_domains(" Example.com, @corp.io ,,"),
            vec!["example.com".to_string(), "corp.io".to_string()]
        );
        assert!(parse_domains("").is_empty());
    }

    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef-session";

    /// A unique, self-deleting file in the system temp directory.
    struct TempSecret(PathBuf);

    impl TempSecret {
        fn new(contents: &[u8]) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "unlinked-session-secret-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, contents).unwrap();
            Self(path)
        }
    }

    impl Drop for TempSecret {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn env_key() -> Key {
        session_key(Some(SECRET.into()), None, false).unwrap()
    }

    fn file_key(contents: &[u8], dev_mode: bool) -> anyhow::Result<Key> {
        let file = TempSecret::new(contents);
        session_key(None, Some(file.0.clone()), dev_mode)
    }

    #[test]
    fn session_secret_file_is_read() {
        let key = file_key(SECRET.as_bytes(), false).unwrap();
        assert_eq!(key.master(), env_key().master());
    }

    #[test]
    fn session_secret_file_strips_one_trailing_newline() {
        let read = |contents: &str| {
            let file = TempSecret::new(contents.as_bytes());
            read_secret_file(&file.0).unwrap()
        };
        assert_eq!(read(&format!("{SECRET}\n")), SECRET.as_bytes());
        assert_eq!(read(&format!("{SECRET}\r\n")), SECRET.as_bytes());
        assert_eq!(read(SECRET), SECRET.as_bytes());
        // Exactly one newline is stripped; anything more is part of the secret.
        assert_eq!(
            read(&format!("{SECRET}\n\n")),
            format!("{SECRET}\n").as_bytes()
        );
        assert_eq!(
            read(&format!("{SECRET}\r\r\n")),
            format!("{SECRET}\r").as_bytes()
        );
        assert_eq!(read("a\nb\n"), b"a\nb");

        // Validation applies after stripping: 63 bytes plus a newline is too short.
        let short = &SECRET[..63];
        assert!(file_key(format!("{short}\n").as_bytes(), false).is_err());
        assert!(file_key(format!("{short}\r\n").as_bytes(), false).is_err());
        let env = env_key();
        for contents in [format!("{SECRET}\n"), format!("{SECRET}\r\n")] {
            let key = file_key(contents.as_bytes(), false).unwrap();
            assert_eq!(key.master(), env.master(), "{contents:?}");
        }
    }

    #[test]
    fn session_secret_file_matches_env_and_is_deterministic() {
        let a = file_key(format!("{SECRET}\n").as_bytes(), false).unwrap();
        let b = file_key(format!("{SECRET}\n").as_bytes(), true).unwrap();
        assert_eq!(a.master(), b.master());
        assert_eq!(a.master(), env_key().master());
        // The env path derives the same key from the same value every time.
        assert_eq!(env_key().master(), env_key().master());
    }

    #[test]
    fn session_secret_and_file_both_set_is_an_error() {
        let file = TempSecret::new(SECRET.as_bytes());
        for dev_mode in [false, true] {
            let err = session_key(Some(SECRET.into()), Some(file.0.clone()), dev_mode)
                .map(|_| ())
                .expect_err("both set must fail");
            assert!(err.to_string().contains("both set"), "{err}");
        }
    }

    #[test]
    fn missing_session_secret_file_is_an_error_in_every_mode() {
        let path = std::env::temp_dir().join("unlinked-session-secret-does-not-exist");
        for dev_mode in [false, true] {
            let err = session_key(None, Some(path.clone()), dev_mode)
                .map(|_| ())
                .expect_err("missing file must fail");
            assert!(err.to_string().contains("could not be read"), "{err}");
        }
    }

    #[test]
    fn short_session_secret_file_is_an_error_in_every_mode() {
        for dev_mode in [false, true] {
            for contents in [&b""[..], b"\n", b"too short\n"] {
                let err = file_key(contents, dev_mode)
                    .map(|_| ())
                    .expect_err("short must fail");
                assert!(err.to_string().contains("at least 64 bytes"), "{err}");
                assert!(!err.to_string().contains("too short"), "{err}");
            }
        }
    }

    #[test]
    fn unset_session_secret_requires_dev_mode() {
        assert!(session_key(None, None, false).is_err());
        assert!(session_key(None, None, true).is_ok());
        assert!(session_key(Some("short".into()), None, true).is_err());
    }

    #[test]
    fn email_allowed_respects_domains() {
        let mut config = crate::test_support::test_config();
        assert!(config.email_allowed("anyone@anywhere.org"));

        config.allowed_email_domains = vec!["example.com".to_string()];
        assert!(config.email_allowed("ada@example.com"));
        assert!(config.email_allowed("ada@EXAMPLE.com"));
        assert!(!config.email_allowed("ada@example.com.evil.org"));
        assert!(!config.email_allowed("ada@sub.example.com"));
        assert!(!config.email_allowed("no-at-sign"));
    }
}
