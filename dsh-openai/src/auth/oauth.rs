//! Loopback PKCE/OIDC login, serialized rotating refresh, and session revocation.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use rand::RngCore;
use reqwest::Url;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    revocation_endpoint: String,
}
#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    token_type: String,
    expires_in: u64,
    scope: String,
    #[serde(default)]
    earliest_refresh_at: u64,
}
impl TokenReply {
    fn credentials(self, previous_id: Option<&str>) -> Result<Credentials> {
        if !self.token_type.eq_ignore_ascii_case("bearer") || self.expires_in == 0 {
            bail!("Invalid OAuth token response.");
        }
        let credentials = Credentials {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            id_token: if self.id_token.is_empty() {
                previous_id.unwrap_or_default().into()
            } else {
                self.id_token
            },
            scopes: self.scope.split_whitespace().map(str::to_owned).collect(),
            expires_at: now().saturating_add(self.expires_in),
            earliest_refresh_at: self.earliest_refresh_at,
        };
        credentials.check_scopes()?;
        Ok(credentials)
    }
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn auth_url(url: &str) -> Result<Url> {
    let url = Url::parse(url).map_err(|_| anyhow!("Invalid OpenAI discovery endpoint."))?;
    if url.scheme() != "https"
        || url.host_str() != Some("auth.openai.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("Discovery endpoint must remain on https://auth.openai.com.");
    }
    Ok(url)
}
async fn discovery(client: &Client) -> Result<Discovery> {
    let data = checked_json(
        client
            .get(format!("{AUTH_ISSUER}/.well-known/openid-configuration"))
            .send()
            .await
            .map_err(|_| anyhow!("Cannot fetch OpenAI discovery."))?,
    )
    .await?;
    let discovery: Discovery =
        serde_json::from_value(data).map_err(|_| anyhow!("Invalid OpenAI discovery."))?;
    if discovery.issuer != AUTH_ISSUER {
        bail!("Unexpected OpenAI issuer.");
    }
    auth_url(&discovery.jwks_uri)?;
    auth_url(&discovery.revocation_endpoint)?;
    Ok(discovery)
}
async fn token(client: &Client, endpoint: &str, form: &[(&str, &str)]) -> Result<TokenReply> {
    let response = client
        .post(endpoint)
        .form(form)
        .send()
        .await
        .map_err(|_| anyhow!("OAuth token request failed."))?;
    serde_json::from_value(checked_json(response).await?)
        .map_err(|_| anyhow!("Invalid OAuth token response."))
}
// Retry revocation only for transport failures or 5xx; invalid requests require user action.
async fn revoke(client: &Client, endpoint: Url, token: &str, client_id: &str) -> Result<()> {
    for attempt in 0..3 {
        let result = client
            .post(endpoint.clone())
            .form(&[
                ("token", token),
                ("token_type_hint", "refresh_token"),
                ("client_id", client_id),
            ])
            .send()
            .await;
        match result {
            Ok(response) if response.status() == reqwest::StatusCode::OK => return Ok(()),
            Ok(response) if !response.status().is_server_error() => {
                bail!("Remote revocation was not confirmed.")
            }
            _ => {}
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
        }
    }
    bail!("Remote revocation was not confirmed.")
}
/// Pending login intentionally has no Debug implementation: URL/PKCE are private.
pub struct LoginAttempt {
    store: AuthStore,
    listener: tokio::net::TcpListener,
    state: String,
    nonce: String,
    verifier: String,
    redirect: String,
    authorization_url: Url,
    selected: Option<Account>,
    pending_id: Option<String>,
}
impl LoginAttempt {
    pub async fn start(
        store: AuthStore,
        new_account: bool,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<Self> {
        let _lock = store.lock(cancel).await?;
        let reg = store.registration_or_new()?;
        let selected = if new_account {
            None
        } else {
            reg.accounts
                .iter()
                .find(|a| Some(&a.label) == reg.active.as_ref())
                .cloned()
                .or_else(|| (reg.accounts.len() == 1).then(|| reg.accounts[0].clone()))
        };
        let pending_id = if selected.is_none() && !new_account {
            store.pending_id()?
        } else {
            None
        };
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let redirect = format!(
            "http://127.0.0.1:{}/auth/callback",
            listener.local_addr()?.port()
        );
        let state = random();
        let nonce = random();
        let verifier = random();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization_url = Url::parse(&format!("{AUTH_ISSUER}/api/accounts/authorize"))?;
        let client_id = selected
            .as_ref()
            .map(|a| a.client_id.as_str())
            .or(pending_id.as_deref())
            .unwrap_or("dynamic_agent_client");
        authorization_url.query_pairs_mut().extend_pairs([
            ("client_id", client_id),
            ("ext_agent_host_id", reg.host_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect.as_str()),
            ("scope", SCOPES),
            ("resource", RESOURCE),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ]);
        if let Some(account) = &selected
            && let Ok(credentials) = store.credentials(account)
            && !credentials.id_token.is_empty()
        {
            authorization_url
                .query_pairs_mut()
                .append_pair("id_token_hint", &credentials.id_token);
        }
        if selected.is_none() && pending_id.is_none() {
            authorization_url
                .query_pairs_mut()
                .append_pair("agent_name_hint", "doge-shell");
        }
        Ok(Self {
            store,
            listener,
            state,
            nonce,
            verifier,
            redirect,
            authorization_url,
            selected,
            pending_id,
        })
    }
    /// Pass directly to the system browser; never log or persist this URL.
    pub fn authorization_url(&self) -> &str {
        self.authorization_url.as_str()
    }
    fn callback(&self, target: &str) -> Result<(String, String)> {
        let url = Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|_| anyhow!("Invalid OAuth callback."))?;
        if url.path() != "/auth/callback" {
            bail!("Unexpected OAuth callback path.");
        }
        let mut fields = std::collections::HashMap::new();
        for (k, v) in url.query_pairs() {
            if fields.insert(k.into_owned(), v.into_owned()).is_some() {
                bail!("Duplicate OAuth callback parameter.");
            }
        }
        // Validate state before even reading code/error/client_id.
        if fields.get("state") != Some(&self.state) {
            bail!("OAuth state mismatch.");
        }
        if fields.contains_key("error") {
            bail!("ChatGPT authorization was declined; start a fresh chat_auth login.");
        }
        let callback_id = fields.get("client_id").filter(|s| !s.is_empty());
        let id = match &self.selected {
            Some(account) => {
                if callback_id.is_some_and(|id| id != &account.client_id) {
                    bail!("OAuth callback changed the selected client registration.");
                }
                account.client_id.clone()
            }
            None if self.pending_id.is_some() => {
                let id = self.pending_id.as_ref().unwrap();
                if callback_id.is_some_and(|v| v != id) {
                    bail!("OAuth callback changed the pending client registration.");
                }
                id.clone()
            }
            None => callback_id
                .filter(|s| s.as_str() != "dynamic_agent_client")
                .cloned()
                .ok_or_else(|| anyhow!("OAuth registration did not return an issued client ID."))?,
        };
        let code = fields
            .get("code")
            .filter(|s| !s.is_empty())
            .cloned()
            .ok_or_else(|| anyhow!("OAuth callback contained no code."))?;
        Ok((code, id))
    }
    pub async fn finish(self, cancel: Option<&dyn Fn() -> bool>) -> Result<Account> {
        wait(self.finish_inner(), cancel, Duration::from_secs(300)).await
    }
    async fn finish_inner(self) -> Result<Account> {
        let (mut connection, _) = self.listener.accept().await?;
        let mut request = vec![0u8; 8192];
        let length = tokio::time::timeout(Duration::from_secs(5), connection.read(&mut request))
            .await
            .map_err(|_| anyhow!("OAuth callback timed out."))??;
        let first = std::str::from_utf8(&request[..length])
            .map_err(|_| anyhow!("Invalid OAuth callback HTTP."))?
            .lines()
            .next()
            .unwrap_or_default();
        let mut words = first.split_whitespace();
        if words.next() != Some("GET") {
            bail!("OAuth callback requires GET.");
        }
        let callback = self.callback(words.next().unwrap_or_default());
        let body = if callback.is_ok() {
            "Return to doge-shell to complete sign-in."
        } else {
            "Sign-in could not be completed. Return to doge-shell."
        };
        let status = if callback.is_ok() {
            "200 OK"
        } else {
            "400 Bad Request"
        };
        connection.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
        drop(connection);
        let (code, client_id) = callback?;
        if self.selected.is_none() {
            let _lock = self.store.lock(None).await?;
            self.store.save("pending-registration.json", &client_id)?;
        }
        let client = http_client()?;
        let reply = token(
            &client,
            self.store.token_endpoint(),
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", &code),
                ("code_verifier", &self.verifier),
                ("redirect_uri", &self.redirect),
                ("resource", RESOURCE),
            ],
        )
        .await?;
        let credentials = reply.credentials(None)?;
        let discovery = discovery(&client).await?;
        let jwks: JwkSet = serde_json::from_value(
            checked_json(
                client
                    .get(auth_url(&discovery.jwks_uri)?)
                    .send()
                    .await
                    .map_err(|_| anyhow!("Cannot fetch OpenAI signing keys."))?,
            )
            .await?,
        )
        .map_err(|_| anyhow!("Invalid OpenAI signing keys."))?;
        let subject = validate_identity(&credentials.id_token, &client_id, &self.nonce, &jwks)?;
        if self.selected.as_ref().is_some_and(|a| a.subject != subject) {
            bail!("Authenticated identity differs from the selected ChatGPT account.");
        }
        let _lock = self.store.lock(None).await?;
        let mut reg = self.store.registrations()?;
        let account = reg
            .accounts
            .iter()
            .find(|a| a.client_id == client_id && a.subject == subject)
            .cloned()
            .unwrap_or_else(|| Account {
                label: uuid::Uuid::new_v4().to_string(),
                client_id,
                subject,
            });
        self.store.save_credentials(&account, &credentials)?;
        if !reg.accounts.iter().any(|a| a.label == account.label) {
            reg.accounts.push(account.clone());
        }
        reg.active = Some(account.label.clone());
        self.store.save("registration.json", &reg)?;
        self.store.clear_pending()?;
        Ok(account)
    }
}
fn validate_identity(
    id_token: &str,
    client_id: &str,
    nonce: &str,
    jwks: &JwkSet,
) -> Result<String> {
    let header = decode_header(id_token).map_err(|_| anyhow!("Invalid ID token header."))?;
    if header.alg != Algorithm::RS256 {
        bail!("Unsupported ID token signature algorithm.");
    }
    let key = header
        .kid
        .as_ref()
        .and_then(|id| jwks.find(id))
        .ok_or_else(|| anyhow!("Unknown ID token signing key."))?;
    let key = DecodingKey::from_jwk(key).map_err(|_| anyhow!("Invalid ID token signing key."))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[AUTH_ISSUER]);
    validation.set_audience(&[client_id]);
    validation.leeway = 0;
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<serde_json::Value>(id_token, &key, &validation)
        .map_err(|_| anyhow!("ID token signature, issuer, audience, or expiry validation failed."))?
        .claims;
    if claims.get("nonce").and_then(|v| v.as_str()) != Some(nonce) {
        bail!("ID token nonce mismatch.");
    }
    claims
        .get("sub")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("ID token contains no account identity."))
}
impl AuthStore {
    pub async fn access(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<(Account, String)> {
        let _lock = self.lock(cancel).await?;
        let account = self.active()?;
        let credentials = self.credentials(&account)?;
        credentials.check_scopes()?;
        if credentials.expires_at > now().saturating_add(60) {
            return Ok((account, credentials.access_token));
        }
        if credentials.earliest_refresh_at > now() {
            if credentials.expires_at > now() {
                return Ok((account, credentials.access_token));
            }
            bail!(
                "Access token expired before refresh is allowed; retry later or run chat_auth login."
            );
        }
        let client = http_client()?;
        let reply = wait(
            token(
                &client,
                self.token_endpoint(),
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", &account.client_id),
                    ("refresh_token", &credentials.refresh_token),
                    ("resource", RESOURCE),
                ],
            ),
            cancel,
            Duration::from_secs(30),
        )
        .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                if error.to_string().contains("code=invalid_grant ")
                    || error.to_string().contains("code=invalid_token ")
                {
                    self.clear_credentials(&account)?;
                }
                return Err(error);
            }
        };
        let rotated = reply.credentials(Some(&credentials.id_token))?;
        self.save_credentials(&account, &rotated)?;
        Ok((account, rotated.access_token))
    }
    pub fn is_active(&self, account: &Account) -> bool {
        self.active().is_ok_and(|a| {
            a.label == account.label
                && a.client_id == account.client_id
                && a.subject == account.subject
        })
    }
    pub async fn logout(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<()> {
        let _lock = self.lock(cancel).await?;
        let account = self.active()?;
        let credentials = self.credentials(&account)?;
        let mut reg = self.registrations()?;
        reg.active = None;
        self.save("registration.json", &reg)?;
        let result = wait(
            async {
                let client = http_client()?;
                let discovery = discovery(&client).await?;
                revoke(
                    &client,
                    auth_url(&discovery.revocation_endpoint)?,
                    &credentials.refresh_token,
                    &account.client_id,
                )
                .await?;
                Ok(())
            },
            cancel,
            Duration::from_secs(30),
        )
        .await;
        self.clear_credentials(&account)?;
        result.map_err(|_| anyhow!("Signed out locally. Remote revocation was not confirmed; disconnect the app in ChatGPT Settings."))
    }
    pub fn select(&self, label: &str) -> Result<()> {
        run(async {
            let _lock = self.lock(None).await?;
            let mut reg = self.registrations()?;
            if !reg.accounts.iter().any(|a| a.label == label) {
                bail!("Unknown ChatGPT account label.");
            }
            reg.active = Some(label.to_string());
            self.save("registration.json", &reg)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_identity_enforces_signature_issuer_audience_expiry_nonce() {
        let key = jsonwebtoken::EncodingKey::from_rsa_der(include_bytes!("fixtures/mock-rsa.der"));
        let jwks: JwkSet = serde_json::from_str(include_str!("fixtures/mock-jwks.json")).unwrap();
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some("mock".into());
        let mut claims = serde_json::json!({"iss":AUTH_ISSUER,"aud":"issued","exp":now()+100,"sub":"mock-account","nonce":"nonce"});
        let encode = |v: &serde_json::Value| jsonwebtoken::encode(&header, v, &key).unwrap();
        assert_eq!(
            validate_identity(&encode(&claims), "issued", "nonce", &jwks).unwrap(),
            "mock-account"
        );
        assert!(validate_identity(&encode(&claims), "other", "nonce", &jwks).is_err());
        assert!(validate_identity(&encode(&claims), "issued", "other", &jwks).is_err());
        claims["exp"] = serde_json::json!(now() - 10);
        assert!(validate_identity(&encode(&claims), "issued", "nonce", &jwks).is_err());
        claims["exp"] = serde_json::json!(now() + 100);
        claims["iss"] = serde_json::json!("https://evil.example");
        assert!(validate_identity(&encode(&claims), "issued", "nonce", &jwks).is_err());
        let mut forged = encode(&claims);
        forged.push('x');
        assert!(validate_identity(&forged, "issued", "nonce", &jwks).is_err());
    }
    #[tokio::test]
    async fn callback_state_issued_id_pkce_and_exact_redirect() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::new(
            std::fs::canonicalize(dir.path())
                .unwrap()
                .join("subscription-auth"),
        );
        let attempt = LoginAttempt::start(store.clone(), false, None)
            .await
            .unwrap();
        assert_eq!(
            attempt.listener.local_addr().unwrap().ip(),
            std::net::Ipv4Addr::LOCALHOST
        );
        let query: std::collections::HashMap<_, _> = attempt
            .authorization_url
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(query["client_id"], "dynamic_agent_client");
        assert_eq!(query["agent_name_hint"], "doge-shell");
        assert_eq!(query["redirect_uri"], attempt.redirect);
        assert_eq!(
            query["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(attempt.verifier.as_bytes()))
        );
        assert!(
            attempt
                .callback("/auth/callback?state=wrong&error=access_denied")
                .unwrap_err()
                .to_string()
                .contains("state")
        );
        assert!(
            attempt
                .callback(&format!("/auth/callback?state={}&code=code", attempt.state))
                .is_err()
        );
        assert!(
            attempt
                .callback(&format!(
                    "/auth/callback?state={}&code=code&client_id=dynamic_agent_client",
                    attempt.state
                ))
                .is_err()
        );
        assert_eq!(
            attempt
                .callback(&format!(
                    "/auth/callback?state={}&code=code&client_id=issued",
                    attempt.state
                ))
                .unwrap(),
            ("code".into(), "issued".into())
        );
        let reg = store.registrations().unwrap();
        let again = LoginAttempt::start(store.clone(), true, None)
            .await
            .unwrap();
        assert_eq!(reg.host_id, store.registrations().unwrap().host_id);
        assert_ne!(again.state, attempt.state);
        assert_ne!(again.nonce, attempt.nonce);
        assert_ne!(again.verifier, attempt.verifier);
        store.save("pending-registration.json", &"issued").unwrap();
        let pending = LoginAttempt::start(store, false, None).await.unwrap();
        assert!(
            !pending
                .authorization_url
                .query_pairs()
                .any(|(key, _)| key == "agent_name_hint")
        );
        assert!(
            pending
                .callback(&format!(
                    "/auth/callback?state={}&code=code&client_id=other",
                    pending.state
                ))
                .is_err()
        );
    }
    #[tokio::test]
    async fn cancellation_and_timeout_do_not_activate_an_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::new(
            std::fs::canonicalize(dir.path())
                .unwrap()
                .join("subscription-auth"),
        );
        let attempt = LoginAttempt::start(store.clone(), true, None)
            .await
            .unwrap();
        assert!(crate::is_ctrl_c_cancelled(
            &attempt.finish(Some(&|| true)).await.unwrap_err()
        ));
        assert!(store.registrations().unwrap().active.is_none());
        let error = wait(
            std::future::pending::<Result<()>>(),
            None,
            Duration::from_millis(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }
    #[test]
    fn endpoints_and_token_permissions_fail_closed() {
        for url in [
            "http://auth.openai.com/keys",
            "https://evil.example/keys",
            "https://auth.openai.com:444/keys",
            "https://secret@auth.openai.com/keys",
        ] {
            assert!(auth_url(url).is_err());
        }
        let reply = TokenReply {
            access_token: "mock-access".into(),
            refresh_token: "mock-refresh".into(),
            id_token: "mock-id".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
            scope: "openid".into(),
            earliest_refresh_at: 0,
        };
        assert!(reply.credentials(None).is_err());
    }
    #[tokio::test]
    async fn returning_login_reuses_id_and_private_identity_hint() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account) =
            super::super::tests::seeded_store(dir.path().join("subscription-auth"));
        let attempt = LoginAttempt::start(store, false, None).await.unwrap();
        let fields: std::collections::HashMap<_, _> = attempt
            .authorization_url
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(fields["client_id"], account.client_id);
        assert_eq!(fields["id_token_hint"], "mock-id");
        assert!(!fields.contains_key("agent_name_hint"));
        assert!(
            attempt
                .callback(&format!(
                    "/auth/callback?state={}&code=mock&client_id=other",
                    attempt.state
                ))
                .is_err()
        );
    }
    #[tokio::test]
    async fn revocation_accepts_empty_200_and_retries_only_transient_errors() {
        use super::super::tests::mock_http;
        let (endpoint, requests, server) = mock_http(vec![
            (503, "text/plain", "".into()),
            (200, "text/plain", "".into()),
        ])
        .await;
        revoke(
            &http_client().unwrap(),
            Url::parse(&endpoint).unwrap(),
            "mock-refresh",
            "mock-issued",
        )
        .await
        .unwrap();
        server.await.unwrap();
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[1].contains("token_type_hint=refresh_token"));
            assert!(!requests[1].contains("client_secret"));
        }
        let (endpoint, requests, server) =
            mock_http(vec![(400, "text/plain", "mock-private-error".into())]).await;
        let error = revoke(
            &http_client().unwrap(),
            Url::parse(&endpoint).unwrap(),
            "mock-refresh",
            "mock-issued",
        )
        .await
        .unwrap_err();
        assert!(!error.to_string().contains("mock-private-error"));
        server.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}
