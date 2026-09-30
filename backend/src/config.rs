use crate::oauth::{OAuthClient, OAuthProviders, Provider};
use oauth2::{basic::BasicClient, AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use std::env;
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
    /// Outside dev mode a `SESSION_SECRET` of at least 64 bytes and at least
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

        let cookie_key = match env::var("SESSION_SECRET") {
            Ok(secret) => {
                tracing::info!("Config: SESSION_SECRET=<set>");
                Key::try_from(secret.as_bytes())
                    .map_err(|_| anyhow::anyhow!("SESSION_SECRET must be at least 64 bytes long"))?
            }
            Err(_) if dev_mode => {
                tracing::warn!(
                    "Config: SESSION_SECRET unset; using a random key (sessions end on restart)"
                );
                Key::generate()
            }
            Err(_) => anyhow::bail!("SESSION_SECRET must be set (at least 64 bytes)"),
        };

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
