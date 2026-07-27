# noise central service deployment

Status: production foundation provisioned; initial protocol and schema specified

Updated: 2026-07-27

## Initial providers

| Boundary | Initial provider |
| --- | --- |
| API and realtime compute | Cyphers VPS |
| Process supervision | systemd |
| TLS reverse proxy | nginx on Cyphers VPS |
| Authoritative metadata and event store | PostgreSQL 16 on Cyphers VPS |
| Durable jobs | PostgreSQL transactional outbox and job tables |
| Encrypted media | Private Cloudflare R2 `noise-media-production` bucket |

There is no staging deployment. Local development uses isolated local
PostgreSQL and object-storage adapters. Releases go directly to production
through a versioned, reversible deployment procedure.

## Why PostgreSQL also owns the initial queue

The first central service does not use Redis, RabbitMQ, or a managed queue.
Jobs are created in the same transaction as the state that requires them.
Workers claim ready jobs using bounded batches and `FOR UPDATE SKIP LOCKED`.

This provides:

- no lost push or maintenance job between a database commit and queue publish;
- durable retries with explicit attempt and next-attempt fields;
- idempotent handlers and a unique deduplication key;
- simple inspection and backup with the authoritative database; and
- one fewer production dependency for a two-person team.

The queue can be split later if measured contention or throughput requires it.
The application contract must not assume that an in-memory notification is
durable.

## Production layout

| Path or interface | Purpose |
| --- | --- |
| `/opt/noise-central` | Root-owned application releases and current symlink |
| `/var/lib/noise-central` | Service-owned local state that cannot live in PostgreSQL |
| `/etc/noise-central/environment` | Root-managed, service-readable production secrets |
| `127.0.0.1:4302` | Planned API and WebSocket listener |
| `127.0.0.1:5432` | PostgreSQL connection |

The service runs as the unprivileged `noise-central` system user. nginx is the
only public HTTP entry point. PostgreSQL is not exposed publicly.

The PostgreSQL foundation contains:

- database `noise`;
- login role `noise_app` with no superuser, role-management, database-creation,
  replication, or row-security-bypass privileges;
- a 20-connection role limit;
- SCRAM-SHA-256 authentication;
- schema `noise`, owned by `noise_app`; and
- no application tables until the canonical schema is reviewed.

The database password exists only in the protected server environment file. It
must not be printed, copied into Git, or embedded in a client build.

The signed installation-session contract is documented in
`DEVICE-SESSIONS.md`. Authentication and renewal are invisible operations
performed by the same installation; another device is never required.

The initial canonical database contract is documented in
`CANONICAL-SCHEMA.md` and implemented by
`deploy/central/migrations/0001_canonical_schema.sql`. On 2026-07-27 that
migration and a representative constraint smoke test passed in a disposable
PostgreSQL database on this host. The disposable database was removed and the
production `noise` schema remained empty. The migration is not applied to
production until a central-service release can consume it.

## Deployment and rollback boundary

The production service should be installed into a versioned release directory.
A `current` symlink selects the active release. A deployment:

1. uploads a complete candidate release;
2. verifies its checksum and ownership;
3. runs backward-compatible database migrations;
4. atomically changes the `current` symlink;
5. restarts the systemd service;
6. verifies loopback health, public health, database connectivity, and
   WebSocket upgrade; and
7. restores the previous symlink and service when verification fails.

Destructive or backward-incompatible migrations require a separately verified
backup and cannot be hidden inside an ordinary application deployment.

## Work still required before traffic

- implement the signed installation-session statements and test vectors in
  `noise-core`;
- implement the central API and transactional schema access layer;
- apply the canonical migration as part of the first reversible service
  deployment, not as an isolated production mutation;
- implement the transactional outbox/job claim contract;
- choose and configure the canonical API hostname;
- create a bucket-scoped R2 runtime credential directly in the protected
  production environment;
- add local PostgreSQL backups plus an encrypted off-host copy and restore
  drill;
- add resource and database monitoring;
- decide whether to add swap after reviewing all workloads on the shared VPS;
  and
- build the compatibility importer and verifier before migrating relay data.

The existing relays continue serving production until the compatibility
service meets the migration acceptance criteria.
