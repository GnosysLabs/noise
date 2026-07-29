use std::{
    collections::HashSet,
    fs,
    io::Cursor,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use a2::{
    Client, ClientConfig, DefaultNotificationBuilder, Endpoint, Error as ApnsError, ErrorReason,
    NotificationBuilder, NotificationOptions, Priority, PushType,
};
use anyhow::{Context, bail};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState, authenticate_session, config::CentralConfig, database::Database, error::ApiError,
};

const TOKEN_ENCRYPTION_CONTEXT: &str = "noise.central.apns-token-encryption.v1";
const TOKEN_LOOKUP_CONTEXT: &str = "noise.central.apns-token-lookup.v1";
const TOKEN_AAD_PREFIX: &[u8] = b"noise.central.apns-token.v1";

#[derive(Clone)]
pub(crate) struct PushService {
    production: Client,
    sandbox: Client,
    token_encryption_key: [u8; 32],
    token_lookup_key: [u8; 32],
    topic: Arc<str>,
}

#[derive(Deserialize)]
pub(crate) struct PushSubscriptionRequest {
    device_token: String,
    environment: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NoisePushMetadata<'a> {
    kind: &'static str,
    sender_public_key: &'a str,
    event_id: &'a str,
}

struct StoredSubscription {
    subscription_pk: i64,
    device_pk: i64,
    environment: String,
    routing_token_ciphertext: Vec<u8>,
}

impl PushService {
    pub(crate) fn open(
        config: &CentralConfig,
        root_secret: &[u8; 32],
    ) -> anyhow::Result<Option<Self>> {
        let Some(push) = config.push_config()? else {
            return Ok(None);
        };
        let key = fs::read(&push.key_file)
            .with_context(|| format!("could not read APNs key {}", push.key_file.display()))?;
        let production = Client::token(
            Cursor::new(key.clone()),
            push.key_id.clone(),
            push.team_id.clone(),
            ClientConfig::new(Endpoint::Production),
        )
        .context("could not initialize production APNs client")?;
        let sandbox = Client::token(
            Cursor::new(key),
            push.key_id,
            push.team_id,
            ClientConfig::new(Endpoint::Sandbox),
        )
        .context("could not initialize sandbox APNs client")?;
        Ok(Some(Self {
            production,
            sandbox,
            token_encryption_key: blake3::derive_key(TOKEN_ENCRYPTION_CONTEXT, root_secret),
            token_lookup_key: blake3::derive_key(TOKEN_LOOKUP_CONTEXT, root_secret),
            topic: Arc::from(push.topic),
        }))
    }

    async fn register(
        &self,
        database: &Database,
        device_pk: i64,
        request: PushSubscriptionRequest,
    ) -> Result<(), ApiError> {
        let device_token = normalize_device_token(&request.device_token)
            .ok_or_else(|| ApiError::bad_request("invalid_push_token"))?;
        let environment = match request.environment.as_str() {
            "production" | "sandbox" => request.environment,
            _ => return Err(ApiError::bad_request("invalid_push_environment")),
        };
        let token_lookup_hash = blake3::keyed_hash(&self.token_lookup_key, device_token.as_bytes());
        let routing_token_ciphertext = encrypt_token(
            &self.token_encryption_key,
            device_pk,
            &environment,
            device_token.as_bytes(),
        )
        .map_err(ApiError::database)?;
        let client = database.pool.get().await.map_err(ApiError::database)?;
        client
            .execute(
                "INSERT INTO noise.push_subscriptions (
                    device_pk, provider, environment, token_lookup_hash,
                    routing_token_ciphertext
                 ) VALUES ($1, 'apns', $2, $3, $4)
                 ON CONFLICT (device_pk, provider) WHERE revoked_at IS NULL
                 DO UPDATE SET
                    environment = EXCLUDED.environment,
                    token_lookup_hash = EXCLUDED.token_lookup_hash,
                    routing_token_ciphertext = EXCLUDED.routing_token_ciphertext,
                    created_at = clock_timestamp(),
                    last_used_at = NULL,
                    revoked_at = NULL",
                &[
                    &device_pk,
                    &environment,
                    &token_lookup_hash.as_bytes().as_slice(),
                    &routing_token_ciphertext,
                ],
            )
            .await
            .map_err(ApiError::database)?;
        Ok(())
    }

    pub(crate) async fn deliver_direct(
        &self,
        database: &Database,
        recipient_account_id: i64,
        sender_public_key: &str,
        event_id: &str,
    ) -> anyhow::Result<usize> {
        let client = database
            .pool
            .get()
            .await
            .context("could not load APNs subscriptions")?;
        let rows = client
            .query(
                "SELECT ps.push_subscription_pk, ps.device_pk, ps.environment,
                        ps.routing_token_ciphertext
                 FROM noise.push_subscriptions ps
                 JOIN noise.devices d ON d.device_pk = ps.device_pk
                 WHERE d.account_id = $1
                   AND d.revoked_at IS NULL
                   AND ps.provider = 'apns'
                   AND ps.revoked_at IS NULL",
                &[&recipient_account_id],
            )
            .await
            .context("could not load APNs subscriptions")?;
        let subscriptions = rows
            .into_iter()
            .map(|row| StoredSubscription {
                subscription_pk: row.get(0),
                device_pk: row.get(1),
                environment: row.get(2),
                routing_token_ciphertext: row.get(3),
            })
            .collect::<Vec<_>>();
        drop(client);

        let mut delivered = 0;
        let mut attempted_tokens = HashSet::new();
        for subscription in subscriptions {
            let token = match decrypt_token(
                &self.token_encryption_key,
                subscription.device_pk,
                &subscription.environment,
                &subscription.routing_token_ciphertext,
            ) {
                Ok(token) => token,
                Err(error) => {
                    eprintln!(
                        "noise-central could not decrypt APNs subscription {}: {error:#}",
                        subscription.subscription_pk
                    );
                    self.revoke(database, subscription.subscription_pk).await?;
                    continue;
                }
            };
            let token = String::from_utf8(token).context("stored APNs token is invalid")?;
            if !attempted_tokens.insert((subscription.environment.clone(), token.clone())) {
                continue;
            }
            let builder = DefaultNotificationBuilder::new()
                .set_title("New message")
                .set_body("sent you a DM")
                .set_sound("default")
                .set_badge(1)
                .set_mutable_content();
            let options = NotificationOptions {
                apns_push_type: Some(PushType::Alert),
                apns_priority: Some(Priority::High),
                apns_topic: Some(self.topic.as_ref()),
                apns_expiration: Some(now_seconds()? + 3_600),
                ..Default::default()
            };
            let mut payload = builder.build(&token, options);
            payload.add_custom_data(
                "noise",
                &NoisePushMetadata {
                    kind: "directMessage",
                    sender_public_key,
                    event_id,
                },
            )?;
            let apns = if subscription.environment == "sandbox" {
                &self.sandbox
            } else {
                &self.production
            };
            match apns.send(payload).await {
                Ok(_) => {
                    delivered += 1;
                    self.mark_used(database, subscription.subscription_pk)
                        .await?;
                }
                Err(error) if permanent_token_error(&error) => {
                    self.revoke(database, subscription.subscription_pk).await?;
                }
                Err(error) => {
                    eprintln!(
                        "noise-central could not deliver APNs subscription {}: {error}",
                        subscription.subscription_pk
                    );
                }
            }
        }
        Ok(delivered)
    }

    async fn mark_used(&self, database: &Database, subscription_pk: i64) -> anyhow::Result<()> {
        let client = database.pool.get().await?;
        client
            .execute(
                "UPDATE noise.push_subscriptions
                 SET last_used_at = clock_timestamp()
                 WHERE push_subscription_pk = $1",
                &[&subscription_pk],
            )
            .await?;
        Ok(())
    }

    async fn revoke(&self, database: &Database, subscription_pk: i64) -> anyhow::Result<()> {
        let client = database.pool.get().await?;
        client
            .execute(
                "UPDATE noise.push_subscriptions
                 SET revoked_at = COALESCE(revoked_at, clock_timestamp())
                 WHERE push_subscription_pk = $1",
                &[&subscription_pk],
            )
            .await?;
        Ok(())
    }
}

pub(crate) async fn register_subscription(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<PushSubscriptionRequest>,
) -> Result<StatusCode, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    let push = state
        .push
        .as_ref()
        .ok_or_else(ApiError::service_unavailable)?;
    push.register(&state.database, session.device_pk, request)
        .await?;
    Ok(StatusCode::ACCEPTED)
}

fn normalize_device_token(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    (normalized.len() == 64
        && normalized
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    .then_some(normalized)
}

fn token_aad(device_pk: i64, environment: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(TOKEN_AAD_PREFIX.len() + 8 + environment.len());
    aad.extend_from_slice(TOKEN_AAD_PREFIX);
    aad.extend_from_slice(&device_pk.to_be_bytes());
    aad.extend_from_slice(environment.as_bytes());
    aad
}

fn encrypt_token(
    key: &[u8; 32],
    device_pk: i64,
    environment: &str,
    token: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let nonce: [u8; 24] = rand::random();
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: token,
                aad: &token_aad(device_pk, environment),
            },
        )
        .map_err(|_| anyhow::anyhow!("could not encrypt APNs token"))?;
    let mut stored = Vec::with_capacity(nonce.len() + ciphertext.len());
    stored.extend_from_slice(&nonce);
    stored.extend_from_slice(&ciphertext);
    Ok(stored)
}

fn decrypt_token(
    key: &[u8; 32],
    device_pk: i64,
    environment: &str,
    stored: &[u8],
) -> anyhow::Result<Vec<u8>> {
    if stored.len() <= 24 {
        bail!("stored APNs token is truncated");
    }
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(&stored[..24]),
            Payload {
                msg: &stored[24..],
                aad: &token_aad(device_pk, environment),
            },
        )
        .map_err(|_| anyhow::anyhow!("stored APNs token authentication failed"))
}

fn permanent_token_error(error: &ApnsError) -> bool {
    matches!(
        error,
        ApnsError::ResponseError(response)
            if response.error.as_ref().is_some_and(|body| matches!(
                body.reason,
                ErrorReason::BadDeviceToken
                    | ErrorReason::DeviceTokenNotForTopic
                    | ErrorReason::Unregistered
            ))
    )
}

fn now_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_tokens_are_normalized_and_validated() {
        let uppercase = "AB".repeat(32);
        assert_eq!(normalize_device_token(&uppercase), Some("ab".repeat(32)));
        assert!(normalize_device_token("abcd").is_none());
        assert!(normalize_device_token(&"xz".repeat(32)).is_none());
    }

    #[test]
    fn stored_device_tokens_are_bound_to_the_device_and_environment() {
        let key = [0x42; 32];
        let token = "ab".repeat(32);
        let stored = encrypt_token(&key, 7, "production", token.as_bytes()).unwrap();
        assert_eq!(
            decrypt_token(&key, 7, "production", &stored).unwrap(),
            token.as_bytes()
        );
        assert!(decrypt_token(&key, 8, "production", &stored).is_err());
        assert!(decrypt_token(&key, 7, "sandbox", &stored).is_err());
    }
}
