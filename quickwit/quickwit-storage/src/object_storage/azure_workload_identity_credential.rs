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

//! Azure Workload Identity (federated token) credential.
//!
//! AKS Workload Identity injects `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, and
//! `AZURE_FEDERATED_TOKEN_FILE`. `azure_identity` 0.21's IMDS path ignores the
//! federated token, so we exchange the projected JWT for an access token the
//! same way the Azure Go SDK / Thanos do.

use std::fs;
use std::sync::Arc;

use async_trait::async_trait;
use azure_core::auth::{AccessToken, Secret, TokenCredential};
use azure_core::error::{Error, ErrorKind};
use azure_core::{HttpClient, Method, Request, Url, from_json};
use azure_identity::TokenCredentialOptions;
use bytes::Bytes;
use serde::Deserialize;
use time::{Duration, OffsetDateTime};

const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const DEFAULT_AUTHORITY_HOST: &str = "https://login.microsoftonline.com/";

#[derive(Debug)]
pub(crate) struct WorkloadIdentityCredential {
    client_id: String,
    tenant_id: String,
    token_file: String,
    authority_host: String,
    http_client: Arc<dyn HttpClient>,
}

impl WorkloadIdentityCredential {
    pub fn from_env() -> azure_core::Result<Self> {
        let client_id = env_non_empty("AZURE_CLIENT_ID")?;
        let tenant_id = env_non_empty("AZURE_TENANT_ID")?;
        let token_file = env_non_empty("AZURE_FEDERATED_TOKEN_FILE")?;
        let authority_host = std::env::var("AZURE_AUTHORITY_HOST")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_AUTHORITY_HOST.to_string());
        let options = TokenCredentialOptions::default();
        Ok(Self {
            client_id,
            tenant_id,
            token_file,
            authority_host,
            http_client: options.http_client(),
        })
    }

    async fn fetch_token(&self, scopes: &[&str]) -> azure_core::Result<AccessToken> {
        let scope = match scopes {
            [scope] => *scope,
            _ => {
                return Err(Error::message(
                    ErrorKind::Credential,
                    "only one scope is supported for workload identity authentication",
                ));
            }
        };
        let assertion = fs::read_to_string(&self.token_file).map_err(|err| {
            Error::message(
                ErrorKind::Credential,
                format!(
                    "failed to read Azure federated token file {}: {err}",
                    self.token_file
                ),
            )
        })?;
        let assertion = assertion.trim();
        if assertion.is_empty() {
            return Err(Error::message(
                ErrorKind::Credential,
                "Azure federated token file is empty",
            ));
        }

        let authority = self.authority_host.trim_end_matches('/');
        let token_url = format!("{authority}/{}/oauth2/v2.0/token", self.tenant_id);
        let url = Url::parse(&token_url).map_err(|err| {
            Error::message(
                ErrorKind::Credential,
                format!("invalid Azure authority token URL {token_url}: {err}"),
            )
        })?;

        let body = format!(
            "client_id={}&scope={}&grant_type=client_credentials&client_assertion_type={}&\
             client_assertion={}",
            percent_encode(&self.client_id),
            percent_encode(scope),
            percent_encode(CLIENT_ASSERTION_TYPE),
            percent_encode(assertion),
        );

        let mut req = Request::new(url, Method::Post);
        req.insert_header("content-type", "application/x-www-form-urlencoded");
        req.set_body(Bytes::from(body));

        let rsp = self.http_client.execute_request(&req).await?;
        let (rsp_status, rsp_headers, rsp_body) = rsp.deconstruct();
        let rsp_body = rsp_body.collect().await?;

        if !rsp_status.is_success() {
            return Err(
                ErrorKind::http_response_from_parts(rsp_status, &rsp_headers, &rsp_body)
                    .into_error(),
            );
        }

        let token_response: EntraTokenResponse = from_json(&rsp_body)?;
        let expires_on = token_response.expires_on.unwrap_or_else(|| {
            OffsetDateTime::now_utc() + Duration::seconds(token_response.expires_in.max(0))
        });
        Ok(AccessToken::new(token_response.access_token, expires_on))
    }
}

#[async_trait]
impl TokenCredential for WorkloadIdentityCredential {
    async fn get_token(&self, scopes: &[&str]) -> azure_core::Result<AccessToken> {
        self.fetch_token(scopes).await
    }

    async fn clear_cache(&self) -> azure_core::Result<()> {
        Ok(())
    }
}

fn env_non_empty(name: &str) -> azure_core::Result<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            Error::message(
                ErrorKind::Credential,
                format!("{name} is required for Azure Workload Identity"),
            )
        })
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct EntraTokenResponse {
    access_token: Secret,
    #[serde(default)]
    expires_in: i64,
    #[serde(default, deserialize_with = "expires_on_opt")]
    expires_on: Option<OffsetDateTime>,
}

fn expires_on_opt<'de, D>(deserializer: D) -> Result<Option<OffsetDateTime>, D::Error>
where D: serde::Deserializer<'de> {
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let ts = match raw {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    };
    Ok(ts.and_then(|v| OffsetDateTime::from_unix_timestamp(v).ok()))
}
