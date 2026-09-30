//! OAuth2 login providers (Google, GitHub) and identity lookup.

use crate::error::ApiError;
use axum::http::header;
use oauth2::{basic::BasicClient, EndpointNotSet, EndpointSet};
use serde::Deserialize;

pub type OAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// GitHub rejects API requests without a `User-Agent`.
const GITHUB_USER_AGENT: &str = "unlinked";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Google,
    GitHub,
}

impl Provider {
    /// The key used in URLs (`/api/auth/login/:provider`) and stored in
    /// `user_identities.provider`.
    pub fn key(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::GitHub => "github",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "google" => Some(Provider::Google),
            "github" => Some(Provider::GitHub),
            _ => None,
        }
    }

    pub fn env_prefix(self) -> &'static str {
        match self {
            Provider::Google => "GOOGLE",
            Provider::GitHub => "GITHUB",
        }
    }

    pub fn auth_url(self) -> &'static str {
        match self {
            Provider::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            Provider::GitHub => "https://github.com/login/oauth/authorize",
        }
    }

    pub fn token_url(self) -> &'static str {
        match self {
            Provider::Google => "https://oauth2.googleapis.com/token",
            Provider::GitHub => "https://github.com/login/oauth/access_token",
        }
    }

    /// `user:email` is needed on GitHub because `/user` omits private
    /// addresses and carries no verification flag.
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            Provider::Google => &["openid", "email", "profile"],
            Provider::GitHub => &["read:user", "user:email"],
        }
    }
}

/// The OAuth clients configured for this deployment. A provider is enabled
/// when all of its credentials are set.
#[derive(Clone, Default)]
pub struct OAuthProviders {
    pub google: Option<OAuthClient>,
    pub github: Option<OAuthClient>,
}

impl OAuthProviders {
    pub fn client(&self, provider: Provider) -> Option<&OAuthClient> {
        match provider {
            Provider::Google => self.google.as_ref(),
            Provider::GitHub => self.github.as_ref(),
        }
    }

    /// Enabled providers, in the order they are offered on the login page.
    pub fn enabled(&self) -> Vec<Provider> {
        [Provider::Google, Provider::GitHub]
            .into_iter()
            .filter(|p| self.client(*p).is_some())
            .collect()
    }
}

/// Who the provider says just signed in. `email` is only set when the
/// provider vouches that the address is verified.
#[derive(Debug, Clone)]
pub struct ProviderIdentity {
    pub provider: Provider,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    sub: String,
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
    name: Option<String>,
    picture: Option<String>,
}

/// `id` is numeric and immutable; the login handle can be renamed.
#[derive(Debug, Deserialize)]
struct GitHubUserInfo {
    id: u64,
    name: Option<String>,
    login: String,
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

/// Ask the provider who owns `access_token`.
pub async fn fetch_identity(
    provider: Provider,
    access_token: &str,
) -> Result<ProviderIdentity, ApiError> {
    let http = reqwest::Client::new();
    match provider {
        Provider::Google => {
            let info: GoogleUserInfo = get_json(
                http.get("https://openidconnect.googleapis.com/v1/userinfo")
                    .bearer_auth(access_token),
            )
            .await?;
            Ok(ProviderIdentity {
                provider,
                subject: info.sub,
                email: info.email.filter(|_| info.email_verified),
                name: info.name,
                avatar_url: info.picture,
            })
        }
        Provider::GitHub => {
            let info: GitHubUserInfo = get_json(
                http.get("https://api.github.com/user")
                    .bearer_auth(access_token)
                    .header(header::USER_AGENT, GITHUB_USER_AGENT),
            )
            .await?;
            let emails: Vec<GitHubEmail> = get_json(
                http.get("https://api.github.com/user/emails")
                    .bearer_auth(access_token)
                    .header(header::USER_AGENT, GITHUB_USER_AGENT),
            )
            .await?;
            Ok(ProviderIdentity {
                provider,
                subject: info.id.to_string(),
                email: primary_verified_email(&emails),
                name: info.name.or(Some(info.login)),
                avatar_url: info.avatar_url,
            })
        }
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, ApiError> {
    request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| ApiError::Internal(format!("identity request failed: {e}")))?
        .json()
        .await
        .map_err(|e| ApiError::Internal(format!("identity response invalid: {e}")))
}

/// The address GitHub marks both primary and verified, if any.
fn primary_verified_email(emails: &[GitHubEmail]) -> Option<String> {
    emails
        .iter()
        .find(|e| e.primary && e.verified)
        .map(|e| e.email.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_keys_roundtrip() {
        for p in [Provider::Google, Provider::GitHub] {
            assert_eq!(Provider::from_key(p.key()), Some(p));
        }
        assert_eq!(Provider::from_key("dev"), None);
    }

    #[test]
    fn github_email_must_be_primary_and_verified() {
        let emails = vec![
            GitHubEmail {
                email: "unverified@x.com".into(),
                primary: true,
                verified: false,
            },
            GitHubEmail {
                email: "secondary@x.com".into(),
                primary: false,
                verified: true,
            },
        ];
        assert_eq!(primary_verified_email(&emails), None);

        let emails = vec![GitHubEmail {
            email: "ok@x.com".into(),
            primary: true,
            verified: true,
        }];
        assert_eq!(primary_verified_email(&emails).as_deref(), Some("ok@x.com"));
    }
}
