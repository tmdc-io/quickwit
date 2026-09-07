// Copyright 2021-Present Datadog, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! In-memory Azure access-token cache.
//!
//! Matches Thanos / the Azure Go SDK (`azidentity`): reuse a token until shortly
//! before expiry, and single-flight refreshes so ingest/search do not stampede
//! IMDS.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration as StdDuration;

use azure_core::auth::AccessToken;
use azure_core::error::{Error, ErrorKind};
use azure_core::StatusCode;
use futures::Future;
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;
use tracing::trace;

/// Refresh this far before `expires_on`, same order as azcore `TokenRefreshOffset`
/// (5m) used by Thanos.
const REFRESH_SKEW: Duration = Duration::minutes(5);

const TOKEN_FETCH_MAX_ATTEMPTS: u32 = 4;

pub(crate) struct TokenCache {
    inner: RwLock<HashMap<Vec<String>, AccessToken>>,
}

impl fmt::Debug for TokenCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenCache").finish_non_exhaustive()
    }
}

impl TokenCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) async fn clear(&self) -> azure_core::Result<()> {
        self.inner.write().await.clear();
        Ok(())
    }

    pub(crate) async fn get_token(
        &self,
        scopes: &[&str],
        callback: impl Future<Output = azure_core::Result<AccessToken>>,
    ) -> azure_core::Result<AccessToken> {
        let scopes_key: Vec<String> = scopes.iter().map(ToString::to_string).collect();
        {
            let cache = self.inner.read().await;
            if let Some(token) = cache.get(&scopes_key) {
                if !is_expired(token) {
                    trace!("returning cached Azure access token");
                    return Ok(token.clone());
                }
            }
        }

        let mut cache = self.inner.write().await;
        if let Some(token) = cache.get(&scopes_key) {
            if !is_expired(token) {
                trace!("returning Azure access token refreshed while waiting on write lock");
                return Ok(token.clone());
            }
        }

        let token = callback.await?;
        cache.insert(scopes_key, token.clone());
        Ok(token)
    }
}

fn is_expired(token: &AccessToken) -> bool {
    token.expires_on < OffsetDateTime::now_utc() + REFRESH_SKEW
}

pub(crate) fn is_transient_token_fetch_error(err: &Error) -> bool {
    match err.kind() {
        ErrorKind::Io => true,
        ErrorKind::HttpResponse { status, .. } => matches!(
            *status,
            StatusCode::TooManyRequests
                | StatusCode::InternalServerError
                | StatusCode::BadGateway
                | StatusCode::ServiceUnavailable
                | StatusCode::GatewayTimeout
        ),
        ErrorKind::Credential => err.to_string().contains("gateway error"),
        _ => false,
    }
}

pub(crate) async fn fetch_token_with_retries<F, Fut>(mut fetch: F) -> azure_core::Result<AccessToken>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = azure_core::Result<AccessToken>>,
{
    let mut last_err: Option<Error> = None;
    for attempt in 1..=TOKEN_FETCH_MAX_ATTEMPTS {
        match fetch().await {
            Ok(token) => return Ok(token),
            Err(err) if is_transient_token_fetch_error(&err) && attempt < TOKEN_FETCH_MAX_ATTEMPTS =>
            {
                let delay = StdDuration::from_millis(200 * 2u64.pow(attempt - 1));
                tracing::warn!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    error = %err,
                    "transient Azure token fetch failure; retrying"
                );
                last_err = Some(err);
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_err.expect("loop only continues after storing last_err"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use azure_core::auth::Secret;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn cache_returns_same_token_until_skew() {
        let cache = TokenCache::new();
        let fetches = Arc::new(AtomicUsize::new(0));
        let expires_on = OffsetDateTime::now_utc() + Duration::minutes(60);

        for expected_count in [1, 1] {
            let fetches_cb = fetches.clone();
            let token = cache
                .get_token(&["https://storage.azure.com/"], async move {
                    fetches_cb.fetch_add(1, Ordering::SeqCst);
                    Ok(AccessToken::new(Secret::new("tok-a"), expires_on))
                })
                .await
                .unwrap();
            assert_eq!(token.token.secret(), "tok-a");
            assert_eq!(fetches.load(Ordering::SeqCst), expected_count);
        }
    }

    #[tokio::test]
    async fn expired_token_is_refreshed() {
        let cache = TokenCache::new();
        let expires_on = OffsetDateTime::now_utc();
        let token = cache
            .get_token(&["https://storage.azure.com/"], async {
                Ok(AccessToken::new(Secret::new("stale"), expires_on))
            })
            .await
            .unwrap();
        assert_eq!(token.token.secret(), "stale");

        let token = cache
            .get_token(&["https://storage.azure.com/"], async {
                Ok(AccessToken::new(
                    Secret::new("fresh"),
                    OffsetDateTime::now_utc() + Duration::minutes(60),
                ))
            })
            .await
            .unwrap();
        assert_eq!(token.token.secret(), "fresh");
    }

    #[tokio::test]
    async fn clear_drops_cached_token() {
        let cache = TokenCache::new();
        let expires_on = OffsetDateTime::now_utc() + Duration::minutes(60);
        cache
            .get_token(&["https://storage.azure.com/"], async {
                Ok(AccessToken::new(Secret::new("first"), expires_on))
            })
            .await
            .unwrap();
        cache.clear().await.unwrap();
        let token = cache
            .get_token(&["https://storage.azure.com/"], async {
                Ok(AccessToken::new(Secret::new("second"), expires_on))
            })
            .await
            .unwrap();
        assert_eq!(token.token.secret(), "second");
    }

    #[test]
    fn token_inside_refresh_skew_is_expired() {
        let token = AccessToken::new(
            Secret::new("x"),
            OffsetDateTime::now_utc() + Duration::minutes(1),
        );
        assert!(is_expired(&token));
        let token = AccessToken::new(
            Secret::new("x"),
            OffsetDateTime::now_utc() + Duration::minutes(10),
        );
        assert!(!is_expired(&token));
    }
}
