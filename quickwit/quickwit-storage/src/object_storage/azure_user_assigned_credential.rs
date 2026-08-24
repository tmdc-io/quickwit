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

//! User-assigned managed identity credential for Azure IMDS.
//!
//! `azure_identity` 0.21's `VirtualMachineManagedIdentityCredential` always
//! requests the *system-assigned* identity and ignores `AZURE_CLIENT_ID`.
//! AKS node pools typically only have user-assigned identities (kubelet UAMI),
//! so `create_credential()` fails at token time with Unauthorized.
//!
//! This credential calls IMDS with an explicit `client_id`, matching how
//! Thanos / the Azure Go SDK honor `AZURE_CLIENT_ID`.

use std::sync::Arc;

use async_trait::async_trait;
use azure_core::auth::{AccessToken, Secret, TokenCredential};
use azure_core::error::{Error, ErrorKind};
use azure_core::{HttpClient, Method, Request, StatusCode, Url, from_json};
use azure_identity::TokenCredentialOptions;
use serde::Deserialize;
use time::OffsetDateTime;

const IMDS_ENDPOINT: &str = "http://169.254.169.254/metadata/identity/oauth2/token";
const IMDS_API_VERSION: &str = "2019-08-01";

#[derive(Debug)]
pub(crate) struct UserAssignedManagedIdentityCredential {
    client_id: String,
    http_client: Arc<dyn HttpClient>,
}

impl UserAssignedManagedIdentityCredential {
    pub fn new(client_id: String) -> Self {
        let options = TokenCredentialOptions::default();
        Self {
            client_id,
            http_client: options.http_client(),
        }
    }

    async fn fetch_token(&self, scopes: &[&str]) -> azure_core::Result<AccessToken> {
        let resource = scopes_to_resource(scopes)?;
        let mut url = Url::parse(IMDS_ENDPOINT).expect("IMDS endpoint is a valid URL");
        url.query_pairs_mut()
            .append_pair("api-version", IMDS_API_VERSION)
            .append_pair("resource", resource)
            .append_pair("client_id", &self.client_id);

        let mut req = Request::new(url, Method::Get);
        req.insert_header("metadata", "true");

        let rsp = self.http_client.execute_request(&req).await?;
        let (rsp_status, rsp_headers, rsp_body) = rsp.deconstruct();
        let rsp_body = rsp_body.collect().await?;

        if !rsp_status.is_success() {
            return match rsp_status {
                StatusCode::BadRequest => Err(Error::message(
                    ErrorKind::Credential,
                    "the requested identity has not been assigned to this resource",
                )),
                StatusCode::BadGateway | StatusCode::GatewayTimeout => Err(Error::message(
                    ErrorKind::Credential,
                    "the request failed due to a gateway error",
                )),
                status => Err(
                    ErrorKind::http_response_from_parts(status, &rsp_headers, &rsp_body)
                        .into_error(),
                ),
            };
        }

        let token_response: MsiTokenResponse = from_json(&rsp_body)?;
        Ok(AccessToken::new(
            token_response.access_token,
            token_response.expires_on,
        ))
    }
}

#[async_trait]
impl TokenCredential for UserAssignedManagedIdentityCredential {
    async fn get_token(&self, scopes: &[&str]) -> azure_core::Result<AccessToken> {
        self.fetch_token(scopes).await
    }

    async fn clear_cache(&self) -> azure_core::Result<()> {
        Ok(())
    }
}

fn scopes_to_resource<'a>(scopes: &'a [&'a str]) -> azure_core::Result<&'a str> {
    if scopes.len() != 1 {
        return Err(Error::message(
            ErrorKind::Credential,
            "only one scope is supported for IMDS authentication",
        ));
    }
    let Some(scope) = scopes.first() else {
        return Err(Error::message(
            ErrorKind::Credential,
            "no scopes were provided",
        ));
    };
    Ok(scope.strip_suffix("/.default").unwrap_or(*scope))
}

#[derive(Debug, Clone, Deserialize)]
struct MsiTokenResponse {
    access_token: Secret,
    #[serde(deserialize_with = "expires_on_string")]
    expires_on: OffsetDateTime,
}

fn expires_on_string<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
where D: serde::Deserializer<'de> {
    let v = String::deserialize(deserializer).map_err(serde::de::Error::custom)?;
    let as_i64 = v.parse::<i64>().map_err(serde::de::Error::custom)?;
    OffsetDateTime::from_unix_timestamp(as_i64).map_err(serde::de::Error::custom)
}
