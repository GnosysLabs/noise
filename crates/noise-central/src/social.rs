use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use noise_core::{GroupDeletion, InviteRecord, InviteRotation, SignedEvent};

use super::{
    AppState, authenticate_session, decode_canonical_array, decode_hex_32, encode_hex,
    error::ApiError,
    watch::{WatchChange, record_watch_change},
};

pub async fn publish_invite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(invite): Json<InviteRecord>,
) -> Result<StatusCode, ApiError> {
    invite
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_invitation"))?;
    let session = authenticate_session(&state, &headers).await?;
    let issuer =
        decode_canonical_array::<32>(&invite.issuer_public_key, "invalid_invitation_issuer")?;
    if issuer != session.identity_public_key {
        return Err(ApiError::bad_request("invitation_issuer_mismatch"));
    }
    let group_id = invite
        .group_id
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("invitation_group_missing"))
        .and_then(|value| decode_hex_32(value, "invalid_group_id"))?;
    let locator = decode_hex_32(&invite.locator, "invalid_invitation_locator")?;
    let wire = serde_json::to_vec(&invite).map_err(ApiError::database)?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let group = client
        .query_opt(
            "SELECT group_pk
             FROM noise.groups
             WHERE protocol_group_id = $1
               AND founder_account_id = $2
               AND lifecycle_state = 'active'",
            &[&group_id.as_slice(), &session.account_id],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    let group_pk: i64 = group.get(0);
    let inserted = client
        .query_opt(
            "INSERT INTO noise.legacy_invitations (
                invitation_id, lookup_hash, group_pk, generation,
                signed_wire_record
             ) VALUES ($1, $1, $2, 0, $3)
             ON CONFLICT (invitation_id) DO UPDATE
             SET accepted_at = noise.legacy_invitations.accepted_at
             WHERE noise.legacy_invitations.group_pk = EXCLUDED.group_pk
               AND noise.legacy_invitations.signed_wire_record
                   = EXCLUDED.signed_wire_record
             RETURNING 1",
            &[&locator.as_slice(), &group_pk, &wire],
        )
        .await
        .map_err(ApiError::database)?;
    if inserted.is_none() {
        return Err(ApiError::conflict("invitation_conflict"));
    }
    Ok(StatusCode::ACCEPTED)
}

pub async fn get_invite(
    State(state): State<Arc<AppState>>,
    Path(locator): Path<String>,
) -> Result<Json<InviteRecord>, ApiError> {
    let locator = decode_hex_32(&locator, "invalid_invitation_locator")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let row = client
        .query_opt(
            "SELECT i.signed_wire_record, latest.signed_wire_record
             FROM noise.legacy_invitations i
             JOIN noise.groups g ON g.group_pk = i.group_pk
             LEFT JOIN LATERAL (
                 SELECT r.signed_wire_record
                 FROM noise.legacy_invite_rotations r
                 JOIN noise.legacy_invitations current
                   ON current.invitation_id = r.invitation_id
                 WHERE current.group_pk = i.group_pk
                 ORDER BY r.generation DESC
                 LIMIT 1
             ) latest ON true
             WHERE i.lookup_hash = $1
               AND g.lifecycle_state = 'active'",
            &[&locator.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("invitation_not_found"))?;
    let invite_wire: Vec<u8> = row.get(0);
    let invite: InviteRecord = serde_json::from_slice(&invite_wire).map_err(ApiError::database)?;
    invite.verify().map_err(ApiError::database)?;
    let rotation_wire: Option<Vec<u8>> = row.get(1);
    if let Some(rotation_wire) = rotation_wire {
        let rotation: InviteRotation =
            serde_json::from_slice(&rotation_wire).map_err(ApiError::database)?;
        rotation.verify().map_err(ApiError::database)?;
        if rotation
            .new_invite
            .as_ref()
            .is_none_or(|current| current.locator != invite.locator)
        {
            return Err(ApiError::not_found("invitation_not_found"));
        }
    }
    Ok(Json(invite))
}

pub async fn publish_invite_rotation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(rotation): Json<InviteRotation>,
) -> Result<StatusCode, ApiError> {
    rotation
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_invite_rotation"))?;
    let session = authenticate_session(&state, &headers).await?;
    let owner =
        decode_canonical_array::<32>(&rotation.owner_public_key, "invalid_invite_rotation_owner")?;
    if owner != session.identity_public_key {
        return Err(ApiError::bad_request("invite_rotation_owner_mismatch"));
    }
    let group_id = decode_hex_32(&rotation.group_id, "invalid_group_id")?;
    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let group = transaction
        .query_opt(
            "SELECT group_pk
             FROM noise.groups
             WHERE protocol_group_id = $1
               AND founder_account_id = $2
               AND lifecycle_state = 'active'
             FOR UPDATE",
            &[&group_id.as_slice(), &session.account_id],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    let group_pk: i64 = group.get(0);
    let generation = rotation.owner_sequence.to_string();
    let invitation_id = if let Some(invite) = rotation.new_invite.as_ref() {
        let locator = decode_hex_32(&invite.locator, "invalid_invitation_locator")?;
        let wire = serde_json::to_vec(invite).map_err(ApiError::database)?;
        transaction
            .execute(
                "INSERT INTO noise.legacy_invitations (
                    invitation_id, lookup_hash, group_pk, generation,
                    signed_wire_record
                 ) VALUES ($1, $1, $2, $3::text::numeric, $4)
                 ON CONFLICT (invitation_id) DO UPDATE
                 SET generation = EXCLUDED.generation,
                     signed_wire_record = EXCLUDED.signed_wire_record
                 WHERE noise.legacy_invitations.group_pk = EXCLUDED.group_pk",
                &[&locator.as_slice(), &group_pk, &generation, &wire],
            )
            .await
            .map_err(ApiError::database)?;
        locator.to_vec()
    } else {
        transaction
            .query_opt(
                "SELECT invitation_id
                 FROM noise.legacy_invitations
                 WHERE group_pk = $1
                 ORDER BY generation DESC, accepted_at DESC
                 LIMIT 1",
                &[&group_pk],
            )
            .await
            .map_err(ApiError::database)?
            .map(|row| row.get::<_, Vec<u8>>(0))
            .ok_or_else(|| ApiError::conflict("invitation_missing"))?
    };
    let wire = serde_json::to_vec(&rotation).map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.legacy_invite_rotations (
                rotation_id, invitation_id, generation, signed_wire_record
             ) VALUES ($1, $2, $3::text::numeric, $4)
             ON CONFLICT (rotation_id) DO UPDATE
             SET invitation_id = EXCLUDED.invitation_id,
                 generation = EXCLUDED.generation,
                 signed_wire_record = EXCLUDED.signed_wire_record,
                 accepted_at = clock_timestamp()
             WHERE noise.legacy_invite_rotations.generation
                 < EXCLUDED.generation",
            &[
                &group_id.as_slice(),
                &invitation_id.as_slice(),
                &generation,
                &wire,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn publish_group_deletion(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(deletion): Json<GroupDeletion>,
) -> Result<StatusCode, ApiError> {
    deletion
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_group_deletion"))?;
    let session = authenticate_session(&state, &headers).await?;
    let owner = decode_canonical_array::<32>(&deletion.owner_public_key, "invalid_group_owner")?;
    if owner != session.identity_public_key {
        return Err(ApiError::bad_request("group_owner_mismatch"));
    }
    let group_id = decode_hex_32(&deletion.group_id, "invalid_group_id")?;
    let signature =
        decode_canonical_array::<64>(&deletion.signature_base64, "invalid_group_deletion")?;
    let deleted_at = deletion.deleted_at_millis.to_string();
    let wire = serde_json::to_vec(&deletion).map_err(ApiError::database)?;
    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let group = transaction
        .query_opt(
            "SELECT group_pk
             FROM noise.groups
             WHERE protocol_group_id = $1
               AND founder_account_id = $2
             FOR UPDATE",
            &[&group_id.as_slice(), &session.account_id],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::forbidden)?;
    let group_pk: i64 = group.get(0);
    transaction
        .execute(
            "INSERT INTO noise.group_deletions (
                group_pk, owner_account_id, deleted_at_millis,
                signature, signed_wire_record
             ) VALUES ($1, $2, $3::text::numeric, $4, $5)
             ON CONFLICT (group_pk) DO UPDATE
             SET deleted_at_millis = GREATEST(
                     noise.group_deletions.deleted_at_millis,
                     EXCLUDED.deleted_at_millis
                 ),
                 signature = CASE
                     WHEN EXCLUDED.deleted_at_millis
                         >= noise.group_deletions.deleted_at_millis
                     THEN EXCLUDED.signature
                     ELSE noise.group_deletions.signature
                 END,
                 signed_wire_record = CASE
                     WHEN EXCLUDED.deleted_at_millis
                         >= noise.group_deletions.deleted_at_millis
                     THEN EXCLUDED.signed_wire_record
                     ELSE noise.group_deletions.signed_wire_record
                 END",
            &[
                &group_pk,
                &session.account_id,
                &deleted_at,
                &signature.as_slice(),
                &wire,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    transaction
        .execute(
            "UPDATE noise.groups
             SET lifecycle_state = 'deleted',
                 deleted_at = COALESCE(deleted_at, clock_timestamp())
             WHERE group_pk = $1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let outbox_payload = serde_json::to_vec(&serde_json::json!({
        "group_id": deletion.group_id,
        "deleted_at_millis": deletion.deleted_at_millis,
    }))
    .map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.outbox_events (
                topic, aggregate_kind, aggregate_id, payload
             ) VALUES ('group.deleted', 'group', $1, $2)",
            &[&group_id.as_slice(), &outbox_payload],
        )
        .await
        .map_err(ApiError::database)?;
    let watch_scope = encode_hex(&group_id);
    record_watch_change(
        &transaction,
        WatchChange {
            scope_id: &watch_scope,
            stream_locator: None,
            control: true,
        },
    )
    .await?;
    transaction.commit().await.map_err(ApiError::database)?;
    state.watch.wake(&watch_scope).await;
    Ok(StatusCode::ACCEPTED)
}

pub async fn publish_contact_signal(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(event): Json<SignedEvent>,
) -> Result<StatusCode, ApiError> {
    event
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_contact_signal"))?;
    if event.encryption_version != 1 || event.epoch.is_some() || event.stream_locator.is_some() {
        return Err(ApiError::bad_request("invalid_contact_signal"));
    }
    let session = authenticate_session(&state, &headers).await?;
    let author = decode_canonical_array::<32>(&event.author_public_key, "invalid_contact_signal")?;
    if author != session.identity_public_key {
        return Err(ApiError::bad_request("contact_signal_author_mismatch"));
    }
    let group_id = decode_hex_32(&event.group_id, "invalid_contact_signal")?;
    let event_id = decode_hex_32(&event.event_id, "invalid_contact_signal")?;
    let sequence = event.author_sequence.to_string();
    let created_at = event.created_at_millis.to_string();
    let wire = serde_json::to_vec(&event).map_err(ApiError::database)?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    client
        .execute(
            "INSERT INTO noise.contact_signals (
                signal_group_id, author_account_id, author_sequence,
                event_id, created_at_millis, signed_wire_record
             ) VALUES ($1, $2, $3::text::numeric, $4, $5::text::numeric, $6)
             ON CONFLICT (signal_group_id, author_account_id) DO UPDATE
             SET author_sequence = EXCLUDED.author_sequence,
                 event_id = EXCLUDED.event_id,
                 created_at_millis = EXCLUDED.created_at_millis,
                 signed_wire_record = EXCLUDED.signed_wire_record,
                 accepted_at = clock_timestamp()
             WHERE noise.contact_signals.author_sequence
                 < EXCLUDED.author_sequence",
            &[
                &group_id.as_slice(),
                &session.account_id,
                &sequence,
                &event_id.as_slice(),
                &created_at,
                &wire,
            ],
        )
        .await
        .map_err(ApiError::database)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn get_contact_signal(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
) -> Result<Json<Vec<SignedEvent>>, ApiError> {
    let group_id = decode_hex_32(&group_id, "invalid_contact_signal")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let rows = client
        .query(
            "SELECT signed_wire_record
             FROM noise.contact_signals
             WHERE signal_group_id = $1
             ORDER BY author_sequence DESC
             LIMIT 8",
            &[&group_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?;
    let events = rows
        .into_iter()
        .map(|row| {
            let wire: Vec<u8> = row.get(0);
            serde_json::from_slice::<SignedEvent>(&wire).map_err(ApiError::database)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if events.is_empty() {
        return Err(ApiError::not_found("contact_signal_not_found"));
    }
    Ok(Json(events))
}
