use std::{
    fs,
    io::Cursor,
    time::{SystemTime, UNIX_EPOCH},
};

use a2::{
    Client, ClientConfig, DefaultNotificationBuilder, Endpoint, NotificationBuilder,
    NotificationOptions, Priority, PushType,
};
use anyhow::{Context, bail};
use noise_core::{DirectPushTrigger, PushSubscriptionRegistration, SignedEvent};
use serde::Serialize;

use crate::{
    config::PushNotificationConfig,
    store::{DurableStore, PushSubscription},
};

const REGISTRATION_MAX_AGE_MILLIS: u64 = 10 * 60 * 1_000;

#[derive(Clone)]
pub struct PushService {
    production: Client,
    sandbox: Client,
    store: DurableStore,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NoisePushMetadata<'a> {
    kind: &'static str,
    sender_public_key: &'a str,
    event_id: &'a str,
}

impl PushService {
    pub fn new(config: &PushNotificationConfig, store: DurableStore) -> anyhow::Result<Self> {
        let key = fs::read(&config.key_file)
            .with_context(|| format!("could not read APNs key {}", config.key_file.display()))?;
        let production = Client::token(
            Cursor::new(key.clone()),
            config.key_id.clone(),
            config.team_id.clone(),
            ClientConfig::new(Endpoint::Production),
        )
        .context("could not initialize production APNs client")?;
        let sandbox = Client::token(
            Cursor::new(key),
            config.key_id.clone(),
            config.team_id.clone(),
            ClientConfig::new(Endpoint::Sandbox),
        )
        .context("could not initialize sandbox APNs client")?;
        Ok(Self {
            production,
            sandbox,
            store,
        })
    }

    pub async fn register(&self, registration: PushSubscriptionRegistration) -> anyhow::Result<()> {
        registration.verify().context("invalid push subscription")?;
        let now = now_millis()?;
        if registration.issued_at_millis.abs_diff(now) > REGISTRATION_MAX_AGE_MILLIS {
            bail!("push subscription proof expired")
        }
        self.store
            .upsert_push_subscription(
                &PushSubscription {
                    mailbox_id: registration.mailbox_id()?,
                    public_key: registration.public_key,
                    installation_id: registration.installation_id,
                    device_token: registration.device_token.to_ascii_lowercase(),
                    environment: registration.environment,
                    topic: registration.topic,
                },
                now,
            )
            .await
    }

    pub async fn deliver(&self, trigger: &DirectPushTrigger) -> anyhow::Result<usize> {
        trigger.verify().context("invalid direct push trigger")?;
        self.deliver_to(
            &trigger.recipient_mailbox_id,
            &trigger.sender_public_key,
            &trigger.event_id,
        )
        .await
    }

    pub async fn deliver_event(&self, event: &SignedEvent) -> anyhow::Result<usize> {
        event.verify().context("invalid direct event")?;
        self.deliver_to(&event.group_id, &event.author_public_key, &event.event_id)
            .await
    }

    async fn deliver_to(
        &self,
        mailbox_id: &str,
        sender_public_key: &str,
        event_id: &str,
    ) -> anyhow::Result<usize> {
        let subscriptions = self.store.push_subscriptions(mailbox_id).await?;
        let mut delivered = 0;
        for subscription in subscriptions {
            if subscription.public_key == sender_public_key {
                continue;
            }
            if !self
                .store
                .claim_push_delivery(
                    &subscription.mailbox_id,
                    &subscription.installation_id,
                    event_id,
                    now_millis()?,
                )
                .await?
            {
                continue;
            }
            let client = if subscription.environment == "sandbox" {
                &self.sandbox
            } else {
                &self.production
            };
            let builder = DefaultNotificationBuilder::new()
                .set_title("New message")
                .set_body("sent you a DM")
                .set_sound("default")
                .set_badge(1)
                .set_mutable_content();
            let options = NotificationOptions {
                apns_push_type: Some(PushType::Alert),
                apns_priority: Some(Priority::High),
                apns_topic: Some(&subscription.topic),
                apns_expiration: Some(now_millis()? / 1_000 + 3_600),
                ..Default::default()
            };
            let mut payload = builder.build(&subscription.device_token, options);
            payload.add_custom_data(
                "noise",
                &NoisePushMetadata {
                    kind: "directMessage",
                    sender_public_key,
                    event_id,
                },
            )?;
            match client.send(payload).await {
                Ok(_) => delivered += 1,
                Err(error) => {
                    self.store
                        .release_push_delivery(
                            &subscription.mailbox_id,
                            &subscription.installation_id,
                            event_id,
                        )
                        .await?;
                    eprintln!(
                        "could not deliver Noise push to {}: {error}",
                        subscription.installation_id
                    );
                }
            }
        }
        Ok(delivered)
    }
}

fn now_millis() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis()
        .try_into()
        .context("system clock value is too large")?)
}
