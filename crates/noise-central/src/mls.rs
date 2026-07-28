use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use deadpool_postgres::{GenericClient, Transaction};
use noise_core::{
    MlsControlLog, MlsEpochRecord, MlsExternalJoinPackage, MlsExternalJoinRequest, MlsGroupGenesis,
    MlsJoinRequest, MlsRemovalReason, MlsRemovalRequest,
};
use tokio_postgres::error::SqlState;

use super::{
    AppState, AuthenticatedSession, authenticate_session, decode_canonical_array, decode_hex_32,
    error::ApiError,
};

const MAX_EPOCH_MEMBERS: usize = 10_000;

pub async fn publish_genesis(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(genesis): Json<MlsGroupGenesis>,
) -> Result<StatusCode, ApiError> {
    genesis
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_mls_genesis"))?;
    let session = authenticate_session(&state, &headers).await?;
    let decoded = DecodedGenesis::new(&genesis)?;
    if decoded.founder_public_key != session.identity_public_key {
        return Err(ApiError::bad_request("mls_author_mismatch"));
    }
    let signed_wire_record = serde_json::to_vec(&genesis).map_err(ApiError::database)?;
    let created_at_millis = genesis.created_at_millis.to_string();

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.groups (protocol_group_id, founder_account_id)
             VALUES ($1, $2)
             ON CONFLICT (protocol_group_id) DO NOTHING",
            &[&decoded.group_id.as_slice(), &session.account_id],
        )
        .await
        .map_err(ApiError::database)?;
    let group = transaction
        .query_opt(
            "SELECT g.group_pk, g.founder_account_id
             FROM noise.groups g
             LEFT JOIN noise.group_restrictions gr
               ON gr.group_pk = g.group_pk
              AND (gr.expires_at IS NULL OR gr.expires_at > clock_timestamp())
             WHERE g.protocol_group_id = $1
               AND g.lifecycle_state = 'active'
               AND g.safety_state = 'normal'
               AND gr.group_pk IS NULL
             FOR UPDATE OF g",
            &[&decoded.group_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("group_unavailable"))?;
    let group_pk: i64 = group.get(0);
    let founder_account_id: Option<i64> = group.get(1);
    if founder_account_id.is_some_and(|founder| founder != session.account_id) {
        return Err(ApiError::conflict("group_founder_conflict"));
    }
    if founder_account_id.is_none() {
        transaction
            .execute(
                "UPDATE noise.groups
                 SET founder_account_id = $2
                 WHERE group_pk = $1",
                &[&group_pk, &session.account_id],
            )
            .await
            .map_err(ApiError::database)?;
    }

    if let Some(existing) = transaction
        .query_opt(
            "SELECT record_id, signed_wire_record
             FROM noise.mls_geneses
             WHERE group_pk = $1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?
    {
        let existing_record_id: Vec<u8> = existing.get(0);
        let existing_wire: Vec<u8> = existing.get(1);
        if existing_record_id == decoded.record_id && existing_wire == signed_wire_record {
            transaction.commit().await.map_err(ApiError::database)?;
            return Ok(StatusCode::OK);
        }
        return Err(ApiError::conflict("mls_genesis_conflict"));
    }

    transaction
        .execute(
            "INSERT INTO noise.mls_geneses (
                record_id, group_pk, founder_account_id, authority_nonce,
                created_at_millis, signature, signed_wire_record
             ) VALUES (
                $1, $2, $3, $4, $5::text::numeric, $6, $7
             )",
            &[
                &decoded.record_id.as_slice(),
                &group_pk,
                &session.account_id,
                &decoded.authority_nonce.as_slice(),
                &created_at_millis,
                &decoded.signature.as_slice(),
                &signed_wire_record,
            ],
        )
        .await
        .map_err(mls_conflict)?;
    let current_cursor = current_cursor(&transaction).await?;
    let membership = transaction
        .query_opt(
            "SELECT membership_pk
             FROM noise.group_memberships
             WHERE group_pk = $1
               AND account_id = $2
               AND active_until_cursor IS NULL
             FOR UPDATE",
            &[&group_pk, &session.account_id],
        )
        .await
        .map_err(ApiError::database)?;
    if let Some(membership) = membership {
        let membership_pk: i64 = membership.get(0);
        transaction
            .execute(
                "UPDATE noise.group_memberships
                 SET role = 'founder'
                 WHERE membership_pk = $1",
                &[&membership_pk],
            )
            .await
            .map_err(ApiError::database)?;
    } else {
        transaction
            .execute(
                "INSERT INTO noise.group_memberships (
                    group_pk, account_id, role, source_kind, source_record_id,
                    active_from_cursor
                 ) VALUES ($1, $2, 'founder', 'mls_genesis', $3, $4)",
                &[
                    &group_pk,
                    &session.account_id,
                    &decoded.record_id.as_slice(),
                    &current_cursor,
                ],
            )
            .await
            .map_err(ApiError::database)?;
    }
    insert_outbox(
        &transaction,
        "mls.genesis.accepted",
        "mls_genesis",
        &decoded.record_id,
        serde_json::json!({
            "group_id": genesis.group_id,
            "record_id": genesis.record_id,
        }),
    )
    .await?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn publish_join_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<MlsJoinRequest>,
) -> Result<StatusCode, ApiError> {
    request
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_mls_join_request"))?;
    let session = authenticate_session(&state, &headers).await?;
    let decoded = DecodedJoinRequest::new(&request)?;
    if decoded.account_public_key != session.identity_public_key {
        return Err(ApiError::bad_request("mls_author_mismatch"));
    }
    let signed_wire_record = serde_json::to_vec(&request).map_err(ApiError::database)?;
    let created_at_millis = request.created_at_millis.to_string();

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let group_pk = control_group(&transaction, &decoded.group_id, true).await?;
    if exact_record_retry(
        &transaction,
        "noise.mls_join_requests",
        "request_id",
        &decoded.request_id,
        &signed_wire_record,
    )
    .await?
    {
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok(StatusCode::OK);
    }
    transaction
        .execute(
            "INSERT INTO noise.mls_join_requests (
                request_id, group_pk, account_id, created_at_millis,
                signature, signed_wire_record
             ) VALUES ($1, $2, $3, $4::text::numeric, $5, $6)",
            &[
                &decoded.request_id.as_slice(),
                &group_pk,
                &session.account_id,
                &created_at_millis,
                &decoded.signature.as_slice(),
                &signed_wire_record,
            ],
        )
        .await
        .map_err(mls_conflict)?;
    insert_outbox(
        &transaction,
        "mls.join_request.accepted",
        "mls_join_request",
        &decoded.request_id,
        serde_json::json!({
            "group_id": request.group_id,
            "request_id": request.request_id,
        }),
    )
    .await?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn publish_removal_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<MlsRemovalRequest>,
) -> Result<StatusCode, ApiError> {
    request
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_mls_removal_request"))?;
    let session = authenticate_session(&state, &headers).await?;
    let decoded = DecodedRemovalRequest::new(&request)?;
    if decoded.requester_public_key != session.identity_public_key {
        return Err(ApiError::bad_request("mls_author_mismatch"));
    }
    let signed_wire_record = serde_json::to_vec(&request).map_err(ApiError::database)?;
    let created_at_millis = request.created_at_millis.to_string();

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let group_pk = control_group(&transaction, &decoded.group_id, true).await?;
    if exact_record_retry(
        &transaction,
        "noise.mls_removal_requests",
        "request_id",
        &decoded.request_id,
        &signed_wire_record,
    )
    .await?
    {
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok(StatusCode::OK);
    }
    member_group(&transaction, &session, &decoded.group_id, false, true).await?;
    let target = transaction
        .query_opt(
            "SELECT a.account_id
             FROM noise.accounts a
             JOIN noise.group_memberships gm
               ON gm.account_id = a.account_id
              AND gm.group_pk = $2
              AND gm.active_until_cursor IS NULL
             WHERE a.identity_public_key = $1
               AND a.status = 'active'",
            &[&decoded.target_public_key.as_slice(), &group_pk],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("group_member_unavailable"))?;
    let target_account_id: i64 = target.get(0);
    let reason = match request.reason {
        MlsRemovalReason::SelfLeft => "self_left",
        MlsRemovalReason::Banned => "banned",
    };
    transaction
        .execute(
            "INSERT INTO noise.mls_removal_requests (
                request_id, group_pk, requester_account_id, target_account_id,
                reason, delete_messages, created_at_millis, signature,
                signed_wire_record
             ) VALUES (
                $1, $2, $3, $4, $5, $6, $7::text::numeric, $8, $9
             )",
            &[
                &decoded.request_id.as_slice(),
                &group_pk,
                &session.account_id,
                &target_account_id,
                &reason,
                &request.delete_messages,
                &created_at_millis,
                &decoded.signature.as_slice(),
                &signed_wire_record,
            ],
        )
        .await
        .map_err(mls_conflict)?;
    insert_outbox(
        &transaction,
        "mls.removal_request.accepted",
        "mls_removal_request",
        &decoded.request_id,
        serde_json::json!({
            "group_id": request.group_id,
            "request_id": request.request_id,
        }),
    )
    .await?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn publish_epoch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(record): Json<MlsEpochRecord>,
) -> Result<StatusCode, ApiError> {
    publish_epoch_inner(state, headers, record, None, None).await
}

pub async fn publish_external_join(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<MlsExternalJoinRequest>,
) -> Result<StatusCode, ApiError> {
    let locator = decode_hex_32(&request.invitation_locator, "invalid_invitation_locator")?;
    publish_epoch_inner(
        state,
        headers,
        request.epoch,
        Some(locator),
        Some(request.next_package),
    )
    .await
}

async fn publish_epoch_inner(
    state: Arc<AppState>,
    headers: HeaderMap,
    record: MlsEpochRecord,
    invitation_locator: Option<[u8; 32]>,
    next_package: Option<MlsExternalJoinPackage>,
) -> Result<StatusCode, ApiError> {
    record
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_mls_epoch"))?;
    if record.external_join != invitation_locator.is_some()
        || record.external_join != next_package.is_some()
    {
        return Err(ApiError::bad_request("invalid_mls_epoch_authorization"));
    }
    if record.member_accounts.len() > MAX_EPOCH_MEMBERS {
        return Err(ApiError::bad_request("mls_epoch_too_many_members"));
    }
    let session = authenticate_session(&state, &headers).await?;
    let decoded = DecodedEpoch::new(&record)?;
    if decoded.author_public_key != session.identity_public_key {
        return Err(ApiError::bad_request("mls_author_mismatch"));
    }
    if let Some(package) = next_package.as_ref() {
        package
            .verify()
            .map_err(|_| ApiError::bad_request("invalid_external_join_package"))?;
        if package.group_id != record.bundle.group_id
            || package.epoch != record.bundle.epoch
            || package.control_record_id != record.record_id
            || package.publisher_public_key != record.owner_public_key
        {
            return Err(ApiError::bad_request("invalid_external_join_package"));
        }
    }
    let signed_wire_record = serde_json::to_vec(&record).map_err(ApiError::database)?;
    let created_at_millis = record.created_at_millis.to_string();
    let parent_epoch = record.bundle.parent_epoch.to_string();
    let epoch = record.bundle.epoch.to_string();

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let group_pk = if let Some(locator) = invitation_locator {
        invited_group(&transaction, &locator, &decoded.group_id, true).await?
    } else {
        member_group(&transaction, &session, &decoded.group_id, true, true).await?
    };
    if exact_record_retry(
        &transaction,
        "noise.mls_epochs",
        "record_id",
        &decoded.record_id,
        &signed_wire_record,
    )
    .await?
    {
        transaction.commit().await.map_err(ApiError::database)?;
        return Ok(StatusCode::OK);
    }

    let genesis_row = transaction
        .query_one(
            "SELECT g.record_id, g.founder_account_id, g.signed_wire_record
             FROM noise.mls_geneses g
             WHERE g.group_pk = $1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let genesis_record_id: Vec<u8> = genesis_row.get(0);
    let founder_account_id: i64 = genesis_row.get(1);
    let genesis_wire: Vec<u8> = genesis_row.get(2);
    let genesis: MlsGroupGenesis =
        serde_json::from_slice(&genesis_wire).map_err(ApiError::database)?;
    genesis.verify().map_err(ApiError::database)?;

    let head = transaction
        .query_opt(
            "SELECT record_id, epoch::text
             FROM noise.mls_epochs
             WHERE group_pk = $1
             ORDER BY noise.mls_epochs.epoch DESC
             LIMIT 1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let (head_record_id, head_epoch, parent_member_ids) = if let Some(head) = head {
        let record_id: Vec<u8> = head.get(0);
        let epoch = head
            .get::<_, &str>(1)
            .parse::<u64>()
            .map_err(ApiError::database)?;
        let rows = transaction
            .query(
                "SELECT m.account_id
                 FROM noise.mls_epoch_members m
                 WHERE m.epoch_record_id = $1
                 ORDER BY m.account_id",
                &[&record_id],
            )
            .await
            .map_err(ApiError::database)?;
        (
            record_id,
            epoch,
            rows.into_iter()
                .map(|row| row.get::<_, i64>(0))
                .collect::<Vec<_>>(),
        )
    } else {
        (genesis_record_id, 0, vec![founder_account_id])
    };
    if decoded.previous_record_id.as_slice() != head_record_id
        || record.bundle.parent_epoch != head_epoch
        || record.bundle.epoch != head_epoch.saturating_add(1)
        || record.bundle.group_id != genesis.group_id
    {
        return Err(ApiError::conflict("mls_control_head_changed"));
    }

    let parent_members = account_public_keys(&transaction, &parent_member_ids).await?;
    if !record.authorizes_from(&genesis.owner_public_key, &parent_members) {
        return Err(ApiError::conflict("mls_epoch_not_authorized"));
    }
    let member_account_ids =
        resolve_member_accounts(&transaction, &record.member_accounts, &parent_member_ids).await?;
    transaction
        .execute(
            "INSERT INTO noise.mls_epochs (
                record_id, group_pk, previous_record_id, parent_epoch, epoch,
                author_account_id, created_at_millis, signature,
                signed_wire_record
             ) VALUES (
                $1, $2, $3, $4::text::numeric, $5::text::numeric, $6,
                $7::text::numeric, $8, $9
             )",
            &[
                &decoded.record_id.as_slice(),
                &group_pk,
                &decoded.previous_record_id.as_slice(),
                &parent_epoch,
                &epoch,
                &session.account_id,
                &created_at_millis,
                &decoded.signature.as_slice(),
                &signed_wire_record,
            ],
        )
        .await
        .map_err(mls_conflict)?;
    transaction
        .execute(
            "INSERT INTO noise.mls_epoch_members (epoch_record_id, account_id)
             SELECT $1, member_id
             FROM unnest($2::bigint[]) AS member_id",
            &[&decoded.record_id.as_slice(), &member_account_ids],
        )
        .await
        .map_err(ApiError::database)?;
    materialize_current_memberships(
        &transaction,
        group_pk,
        founder_account_id,
        &decoded.record_id,
        &member_account_ids,
    )
    .await?;
    if let Some(package) = next_package {
        let package_id = decode_hex_32(&package.package_id, "invalid_external_join_package")?;
        let control_record_id =
            decode_hex_32(&package.control_record_id, "invalid_external_join_package")?;
        let package_epoch = package.epoch.to_string();
        let package_wire = serde_json::to_vec(&package).map_err(ApiError::database)?;
        transaction
            .execute(
                "INSERT INTO noise.mls_external_join_packages (
                    package_id, group_pk, epoch, control_record_id,
                    publisher_account_id, signed_wire_record
                 ) VALUES ($1, $2, $3::text::numeric, $4, $5, $6)
                 ON CONFLICT (group_pk) DO UPDATE
                 SET package_id = EXCLUDED.package_id,
                     epoch = EXCLUDED.epoch,
                     control_record_id = EXCLUDED.control_record_id,
                     publisher_account_id = EXCLUDED.publisher_account_id,
                     signed_wire_record = EXCLUDED.signed_wire_record,
                     accepted_at = clock_timestamp()",
                &[
                    &package_id.as_slice(),
                    &group_pk,
                    &package_epoch,
                    &control_record_id.as_slice(),
                    &session.account_id,
                    &package_wire,
                ],
            )
            .await
            .map_err(mls_conflict)?;
    }
    insert_outbox(
        &transaction,
        "mls.epoch.accepted",
        "mls_epoch",
        &decoded.record_id,
        serde_json::json!({
            "epoch": record.bundle.epoch,
            "group_id": record.bundle.group_id,
            "record_id": record.record_id,
        }),
    )
    .await?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn publish_external_join_package(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(package): Json<MlsExternalJoinPackage>,
) -> Result<StatusCode, ApiError> {
    package
        .verify()
        .map_err(|_| ApiError::bad_request("invalid_external_join_package"))?;
    let session = authenticate_session(&state, &headers).await?;
    let publisher = decode_canonical_array::<32>(
        &package.publisher_public_key,
        "invalid_external_join_package",
    )?;
    if publisher != session.identity_public_key {
        return Err(ApiError::bad_request("mls_author_mismatch"));
    }
    let group_id = decode_hex_32(&package.group_id, "invalid_external_join_package")?;
    let package_id = decode_hex_32(&package.package_id, "invalid_external_join_package")?;
    let control_record_id =
        decode_hex_32(&package.control_record_id, "invalid_external_join_package")?;
    let epoch = package.epoch.to_string();
    let wire = serde_json::to_vec(&package).map_err(ApiError::database)?;

    let mut client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let transaction = client.transaction().await.map_err(ApiError::database)?;
    let group_pk = member_group(&transaction, &session, &group_id, true, true).await?;
    let head = transaction
        .query_opt(
            "SELECT record_id, epoch::text
             FROM noise.mls_epochs
             WHERE group_pk = $1
             ORDER BY noise.mls_epochs.epoch DESC
             LIMIT 1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let (head_record_id, head_epoch) = if let Some(head) = head {
        let record_id: Vec<u8> = head.get(0);
        let epoch = head
            .get::<_, &str>(1)
            .parse::<u64>()
            .map_err(ApiError::database)?;
        (record_id, epoch)
    } else {
        let genesis = transaction
            .query_one(
                "SELECT record_id
                 FROM noise.mls_geneses
                 WHERE group_pk = $1",
                &[&group_pk],
            )
            .await
            .map_err(ApiError::database)?;
        (genesis.get(0), 0)
    };
    if control_record_id.as_slice() != head_record_id || package.epoch != head_epoch {
        eprintln!(
            "[noise-central] external join package head mismatch: \
             group={} package_epoch={} head_epoch={}",
            package.group_id,
            package.epoch,
            head_epoch,
        );
        return Err(ApiError::conflict("mls_control_head_changed"));
    }
    transaction
        .execute(
            "INSERT INTO noise.mls_external_join_packages (
                package_id, group_pk, epoch, control_record_id,
                publisher_account_id, signed_wire_record
             ) VALUES ($1, $2, $3::text::numeric, $4, $5, $6)
             ON CONFLICT (group_pk) DO UPDATE
             SET package_id = EXCLUDED.package_id,
                 epoch = EXCLUDED.epoch,
                 control_record_id = EXCLUDED.control_record_id,
                 publisher_account_id = EXCLUDED.publisher_account_id,
                 signed_wire_record = EXCLUDED.signed_wire_record,
                 accepted_at = clock_timestamp()",
            &[
                &package_id.as_slice(),
                &group_pk,
                &epoch,
                &control_record_id.as_slice(),
                &session.account_id,
                &wire,
            ],
        )
        .await
        .map_err(mls_conflict)?;
    transaction.commit().await.map_err(ApiError::database)?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn external_join_package_by_invite(
    State(state): State<Arc<AppState>>,
    Path(locator): Path<String>,
    headers: HeaderMap,
) -> Result<Json<MlsExternalJoinPackage>, ApiError> {
    let _session = authenticate_session(&state, &headers).await?;
    let locator = decode_hex_32(&locator, "invalid_invitation_locator")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let row = client
        .query_opt(
            "SELECT p.signed_wire_record
             FROM noise.legacy_invitations i
             JOIN noise.groups g ON g.group_pk = i.group_pk
             JOIN noise.mls_external_join_packages p ON p.group_pk = g.group_pk
             LEFT JOIN noise.legacy_invite_rotations r
               ON r.rotation_id = g.protocol_group_id
             LEFT JOIN noise.group_restrictions gr
               ON gr.group_pk = g.group_pk
              AND (gr.expires_at IS NULL OR gr.expires_at > clock_timestamp())
             WHERE i.lookup_hash = $1
               AND (r.invitation_id IS NULL OR r.invitation_id = i.invitation_id)
               AND g.lifecycle_state = 'active'
               AND g.safety_state = 'normal'
               AND gr.group_pk IS NULL",
            &[&locator.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("external_join_package_unavailable"))?;
    let wire: Vec<u8> = row.get(0);
    let package: MlsExternalJoinPackage =
        serde_json::from_slice(&wire).map_err(ApiError::database)?;
    package.verify().map_err(ApiError::database)?;
    Ok(Json(package))
}

pub async fn external_join_package_for_member(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<MlsExternalJoinPackage>, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    let group_id = decode_hex_32(&group_id, "invalid_group_id")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let group_pk = member_group(&client, &session, &group_id, false, true).await?;
    let row = client
        .query_opt(
            "SELECT signed_wire_record
             FROM noise.mls_external_join_packages
             WHERE group_pk = $1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::not_found("external_join_package_unavailable"))?;
    let wire: Vec<u8> = row.get(0);
    let package: MlsExternalJoinPackage =
        serde_json::from_slice(&wire).map_err(ApiError::database)?;
    package.verify().map_err(ApiError::database)?;
    Ok(Json(package))
}

pub async fn control_log(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<MlsControlLog>, ApiError> {
    let _session = authenticate_session(&state, &headers).await?;
    let group_id = decode_hex_32(&group_id, "invalid_group_id")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let group_pk = control_group(&client, &group_id, false).await?;
    let genesis = client
        .query_one(
            "SELECT signed_wire_record
             FROM noise.mls_geneses
             WHERE group_pk = $1",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let genesis_wire: Vec<u8> = genesis.get(0);
    let genesis: MlsGroupGenesis =
        serde_json::from_slice(&genesis_wire).map_err(ApiError::database)?;
    let epoch_rows = client
        .query(
            "SELECT signed_wire_record
             FROM noise.mls_epochs
             WHERE group_pk = $1
             ORDER BY epoch ASC",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let epochs = epoch_rows
        .into_iter()
        .map(|row| {
            let wire: Vec<u8> = row.get(0);
            serde_json::from_slice(&wire).map_err(ApiError::database)
        })
        .collect::<Result<Vec<MlsEpochRecord>, ApiError>>()?;
    let log = MlsControlLog { genesis, epochs };
    log.verify().map_err(ApiError::database)?;
    Ok(Json(log))
}

pub async fn join_requests(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<MlsJoinRequest>>, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    let group_id = decode_hex_32(&group_id, "invalid_group_id")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let group_pk = member_group(&client, &session, &group_id, false, true).await?;
    let rows = client
        .query(
            "SELECT signed_wire_record
             FROM noise.mls_join_requests
             WHERE group_pk = $1
             ORDER BY created_at_millis ASC, request_id ASC",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let requests = decode_records::<MlsJoinRequest>(rows)?;
    for request in &requests {
        request.verify().map_err(ApiError::database)?;
    }
    Ok(Json(requests))
}

pub async fn removal_requests(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<MlsRemovalRequest>>, ApiError> {
    let session = authenticate_session(&state, &headers).await?;
    let group_id = decode_hex_32(&group_id, "invalid_group_id")?;
    let client = state
        .database
        .pool
        .get()
        .await
        .map_err(ApiError::database)?;
    let group_pk = member_group(&client, &session, &group_id, false, true).await?;
    let rows = client
        .query(
            "SELECT signed_wire_record
             FROM noise.mls_removal_requests
             WHERE group_pk = $1
             ORDER BY created_at_millis ASC, request_id ASC",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let requests = decode_records::<MlsRemovalRequest>(rows)?;
    for request in &requests {
        request.verify().map_err(ApiError::database)?;
    }
    Ok(Json(requests))
}

struct DecodedGenesis {
    record_id: [u8; 32],
    group_id: [u8; 32],
    founder_public_key: [u8; 32],
    authority_nonce: [u8; 32],
    signature: [u8; 64],
}

impl DecodedGenesis {
    fn new(genesis: &MlsGroupGenesis) -> Result<Self, ApiError> {
        Ok(Self {
            record_id: decode_hex_32(&genesis.record_id, "invalid_mls_genesis")?,
            group_id: decode_hex_32(&genesis.group_id, "invalid_mls_genesis")?,
            founder_public_key: decode_canonical_array::<32>(
                &genesis.owner_public_key,
                "invalid_mls_genesis",
            )?,
            authority_nonce: decode_canonical_array::<32>(
                &genesis.authority_nonce_base64,
                "invalid_mls_genesis",
            )?,
            signature: decode_canonical_array::<64>(
                &genesis.signature_base64,
                "invalid_mls_genesis",
            )?,
        })
    }
}

struct DecodedJoinRequest {
    request_id: [u8; 32],
    group_id: [u8; 32],
    account_public_key: [u8; 32],
    signature: [u8; 64],
}

impl DecodedJoinRequest {
    fn new(request: &MlsJoinRequest) -> Result<Self, ApiError> {
        Ok(Self {
            request_id: decode_hex_32(&request.request_id, "invalid_mls_join_request")?,
            group_id: decode_hex_32(&request.group_id, "invalid_mls_join_request")?,
            account_public_key: decode_canonical_array::<32>(
                &request.account_public_key,
                "invalid_mls_join_request",
            )?,
            signature: decode_canonical_array::<64>(
                &request.signature_base64,
                "invalid_mls_join_request",
            )?,
        })
    }
}

struct DecodedRemovalRequest {
    request_id: [u8; 32],
    group_id: [u8; 32],
    requester_public_key: [u8; 32],
    target_public_key: [u8; 32],
    signature: [u8; 64],
}

impl DecodedRemovalRequest {
    fn new(request: &MlsRemovalRequest) -> Result<Self, ApiError> {
        Ok(Self {
            request_id: decode_hex_32(&request.request_id, "invalid_mls_removal_request")?,
            group_id: decode_hex_32(&request.group_id, "invalid_mls_removal_request")?,
            requester_public_key: decode_canonical_array::<32>(
                &request.requester_public_key,
                "invalid_mls_removal_request",
            )?,
            target_public_key: decode_canonical_array::<32>(
                &request.target_public_key,
                "invalid_mls_removal_request",
            )?,
            signature: decode_canonical_array::<64>(
                &request.signature_base64,
                "invalid_mls_removal_request",
            )?,
        })
    }
}

struct DecodedEpoch {
    record_id: [u8; 32],
    previous_record_id: [u8; 32],
    group_id: [u8; 32],
    author_public_key: [u8; 32],
    signature: [u8; 64],
}

impl DecodedEpoch {
    fn new(record: &MlsEpochRecord) -> Result<Self, ApiError> {
        Ok(Self {
            record_id: decode_hex_32(&record.record_id, "invalid_mls_epoch")?,
            previous_record_id: decode_hex_32(&record.previous_record_id, "invalid_mls_epoch")?,
            group_id: decode_hex_32(&record.bundle.group_id, "invalid_mls_epoch")?,
            author_public_key: decode_canonical_array::<32>(
                &record.owner_public_key,
                "invalid_mls_epoch",
            )?,
            signature: decode_canonical_array::<64>(&record.signature_base64, "invalid_mls_epoch")?,
        })
    }
}

async fn control_group<C: GenericClient + Sync>(
    client: &C,
    group_id: &[u8; 32],
    lock: bool,
) -> Result<i64, ApiError> {
    let lock_clause = if lock { " FOR UPDATE OF g" } else { "" };
    let statement = format!(
        "SELECT g.group_pk
         FROM noise.groups g
         JOIN noise.mls_geneses mg ON mg.group_pk = g.group_pk
         LEFT JOIN noise.group_restrictions gr
           ON gr.group_pk = g.group_pk
          AND (gr.expires_at IS NULL OR gr.expires_at > clock_timestamp())
         WHERE g.protocol_group_id = $1
           AND g.lifecycle_state = 'active'
           AND g.safety_state = 'normal'
           AND gr.group_pk IS NULL{lock_clause}"
    );
    client
        .query_opt(&statement, &[&group_id.as_slice()])
        .await
        .map_err(ApiError::database)?
        .map(|row| row.get(0))
        .ok_or_else(|| ApiError::not_found("group_unavailable"))
}

async fn invited_group<C: GenericClient + Sync>(
    client: &C,
    invitation_locator: &[u8; 32],
    group_id: &[u8; 32],
    lock: bool,
) -> Result<i64, ApiError> {
    let lock_clause = if lock { " FOR UPDATE OF g" } else { "" };
    let statement = format!(
        "SELECT g.group_pk
         FROM noise.legacy_invitations i
         JOIN noise.groups g ON g.group_pk = i.group_pk
         JOIN noise.mls_geneses mg ON mg.group_pk = g.group_pk
         LEFT JOIN noise.legacy_invite_rotations r
           ON r.rotation_id = g.protocol_group_id
         LEFT JOIN noise.group_restrictions gr
           ON gr.group_pk = g.group_pk
          AND (gr.expires_at IS NULL OR gr.expires_at > clock_timestamp())
         WHERE i.lookup_hash = $1
           AND g.protocol_group_id = $2
           AND (r.invitation_id IS NULL OR r.invitation_id = i.invitation_id)
           AND g.lifecycle_state = 'active'
           AND g.safety_state = 'normal'
           AND gr.group_pk IS NULL{lock_clause}"
    );
    client
        .query_opt(
            &statement,
            &[&invitation_locator.as_slice(), &group_id.as_slice()],
        )
        .await
        .map_err(ApiError::database)?
        .map(|row| row.get(0))
        .ok_or_else(|| ApiError::not_found("invitation_not_found"))
}

async fn member_group<C: GenericClient + Sync>(
    client: &C,
    session: &AuthenticatedSession,
    group_id: &[u8; 32],
    lock: bool,
    require_genesis: bool,
) -> Result<i64, ApiError> {
    let lock_clause = if lock { " FOR UPDATE OF g" } else { "" };
    let genesis_clause = if require_genesis {
        " AND EXISTS (
            SELECT 1 FROM noise.mls_geneses mg WHERE mg.group_pk = g.group_pk
          )"
    } else {
        ""
    };
    let statement = format!(
        "SELECT g.group_pk
         FROM noise.groups g
         JOIN noise.group_memberships gm
           ON gm.group_pk = g.group_pk
          AND gm.account_id = $2
          AND gm.active_until_cursor IS NULL
         LEFT JOIN noise.group_restrictions gr
           ON gr.group_pk = g.group_pk
          AND (gr.expires_at IS NULL OR gr.expires_at > clock_timestamp())
         WHERE g.protocol_group_id = $1
           AND g.lifecycle_state = 'active'
           AND g.safety_state = 'normal'
           AND gr.group_pk IS NULL
           {genesis_clause}{lock_clause}"
    );
    client
        .query_opt(&statement, &[&group_id.as_slice(), &session.account_id])
        .await
        .map_err(ApiError::database)?
        .map(|row| row.get(0))
        .ok_or_else(|| ApiError::not_found("group_unavailable"))
}

async fn exact_record_retry(
    transaction: &Transaction<'_>,
    table: &'static str,
    id_column: &'static str,
    record_id: &[u8; 32],
    signed_wire_record: &[u8],
) -> Result<bool, ApiError> {
    let statement =
        format!("SELECT signed_wire_record FROM {table} WHERE {id_column} = $1 FOR UPDATE");
    let existing = transaction
        .query_opt(&statement, &[&record_id.as_slice()])
        .await
        .map_err(ApiError::database)?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    let existing_wire: Vec<u8> = existing.get(0);
    if existing_wire != signed_wire_record {
        return Err(ApiError::conflict("mls_record_conflict"));
    }
    Ok(true)
}

async fn current_cursor(transaction: &Transaction<'_>) -> Result<i64, ApiError> {
    transaction
        .query_one(
            "SELECT last_cursor FROM noise.cursor_clock WHERE singleton",
            &[],
        )
        .await
        .map(|row| row.get(0))
        .map_err(ApiError::database)
}

async fn account_public_keys(
    transaction: &Transaction<'_>,
    account_ids: &[i64],
) -> Result<Vec<String>, ApiError> {
    let rows = transaction
        .query(
            "SELECT account_id, identity_public_key
             FROM noise.accounts
             WHERE account_id = ANY($1::bigint[])",
            &[&account_ids],
        )
        .await
        .map_err(ApiError::database)?;
    if rows.len() != account_ids.len() {
        return Err(ApiError::conflict("mls_parent_members_unavailable"));
    }
    let keys = rows
        .into_iter()
        .map(|row| {
            let account_id: i64 = row.get(0);
            let key: Vec<u8> = row.get(1);
            (account_id, STANDARD_NO_PAD.encode(key))
        })
        .collect::<HashMap<_, _>>();
    account_ids
        .iter()
        .map(|account_id| {
            keys.get(account_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("mls_parent_members_unavailable"))
        })
        .collect()
}

async fn resolve_member_accounts(
    transaction: &Transaction<'_>,
    member_public_keys: &[String],
    parent_member_ids: &[i64],
) -> Result<Vec<i64>, ApiError> {
    let decoded = member_public_keys
        .iter()
        .map(|key| decode_canonical_array::<32>(key, "invalid_mls_epoch").map(|key| key.to_vec()))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = transaction
        .query(
            "SELECT account_id, identity_public_key, status
             FROM noise.accounts
             WHERE identity_public_key = ANY($1::bytea[])",
            &[&decoded],
        )
        .await
        .map_err(ApiError::database)?;
    if rows.len() != decoded.len() {
        return Err(ApiError::conflict("mls_member_account_unavailable"));
    }
    let accounts = rows
        .into_iter()
        .map(|row| {
            let account_id: i64 = row.get(0);
            let key: Vec<u8> = row.get(1);
            let status: &str = row.get(2);
            (key, (account_id, status == "active"))
        })
        .collect::<HashMap<_, _>>();
    decoded
        .iter()
        .map(|key| {
            let (account_id, active) = accounts
                .get(key)
                .copied()
                .ok_or_else(|| ApiError::conflict("mls_member_account_unavailable"))?;
            if !active && !parent_member_ids.contains(&account_id) {
                return Err(ApiError::conflict("mls_member_account_unavailable"));
            }
            Ok(account_id)
        })
        .collect()
}

async fn materialize_current_memberships(
    transaction: &Transaction<'_>,
    group_pk: i64,
    founder_account_id: i64,
    source_record_id: &[u8; 32],
    member_account_ids: &[i64],
) -> Result<(), ApiError> {
    if !member_account_ids.contains(&founder_account_id) {
        return Err(ApiError::conflict("mls_founder_removed"));
    }
    let current_rows = transaction
        .query(
            "SELECT membership_pk, account_id
             FROM noise.group_memberships
             WHERE group_pk = $1 AND active_until_cursor IS NULL
             FOR UPDATE",
            &[&group_pk],
        )
        .await
        .map_err(ApiError::database)?;
    let current = current_rows
        .into_iter()
        .map(|row| (row.get::<_, i64>(1), row.get::<_, i64>(0)))
        .collect::<HashMap<_, _>>();
    let next = member_account_ids.iter().copied().collect::<HashSet<_>>();
    let removed = current
        .keys()
        .filter(|account_id| !next.contains(account_id))
        .copied()
        .collect::<Vec<_>>();
    let added = next
        .iter()
        .filter(|account_id| !current.contains_key(account_id))
        .copied()
        .collect::<Vec<_>>();
    let cursor = current_cursor(transaction).await?;
    if !removed.is_empty() {
        transaction
            .execute(
                "UPDATE noise.group_memberships
                 SET active_until_cursor = $3
                 WHERE group_pk = $1
                   AND account_id = ANY($2::bigint[])
                   AND active_until_cursor IS NULL",
                &[&group_pk, &removed, &cursor],
            )
            .await
            .map_err(ApiError::database)?;
    }
    if !added.is_empty() {
        transaction
            .execute(
                "INSERT INTO noise.group_memberships (
                    group_pk, account_id, role, source_kind, source_record_id,
                    active_from_cursor
                 )
                 SELECT $1, account_id, 'member', 'mls_epoch', $3, $4
                 FROM unnest($2::bigint[]) AS account_id",
                &[&group_pk, &added, &source_record_id.as_slice(), &cursor],
            )
            .await
            .map_err(ApiError::database)?;
    }
    Ok(())
}

async fn insert_outbox(
    transaction: &Transaction<'_>,
    topic: &'static str,
    aggregate_kind: &'static str,
    aggregate_id: &[u8; 32],
    payload: serde_json::Value,
) -> Result<(), ApiError> {
    let payload = serde_json::to_vec(&payload).map_err(ApiError::database)?;
    transaction
        .execute(
            "INSERT INTO noise.outbox_events (
                topic, aggregate_kind, aggregate_id, payload
             ) VALUES ($1, $2, $3, $4)",
            &[&topic, &aggregate_kind, &aggregate_id.as_slice(), &payload],
        )
        .await
        .map_err(ApiError::database)?;
    Ok(())
}

fn decode_records<T>(rows: Vec<tokio_postgres::Row>) -> Result<Vec<T>, ApiError>
where
    T: serde::de::DeserializeOwned,
{
    rows.into_iter()
        .map(|row| {
            let wire: Vec<u8> = row.get(0);
            serde_json::from_slice(&wire).map_err(ApiError::database)
        })
        .collect()
}

fn mls_conflict(error: tokio_postgres::Error) -> ApiError {
    if error
        .as_db_error()
        .is_some_and(|database| database.code() == &SqlState::UNIQUE_VIOLATION)
    {
        ApiError::conflict("mls_control_head_changed")
    } else {
        ApiError::database(error)
    }
}
