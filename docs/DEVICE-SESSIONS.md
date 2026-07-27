# noise device sessions

Status: version-one signing primitives and central authentication endpoints
implemented; client integration and production deployment pending

Updated: 2026-07-27

## Product invariant

Centralizing noise must not add a sign-in step or change the account UX.

- An installation authenticates and renews its service session silently in the
  background.
- A user is never asked to approve a session from another device.
- A user is never shown a device code, renewal prompt, periodic password
  prompt, or session-expiration screen.
- Restoring noise on a new installation continues to use the existing noise ID
  and password flow on that installation.
- The account password, vault key, identity private key, and device private key
  never leave the installation.
- A remotely revoked installation is logged out using the existing
  `this device was logged out remotely` behavior.

The phrase "device-signed request" always means a request signed by **the same
installation making the request**. It never means that another device must be
available.

## Keys and existing records

Each noise installation gets a dedicated Ed25519 service-authentication key
pair:

- the private key is generated locally and stored using the platform's secure
  local storage;
- the public key is registered with the central service;
- the account identity key signs the registration binding; and
- the service-authentication key signs session challenges.

This is an internal transport credential. It is not:

- the existing `DeviceRecord.device_id`, which is synchronized account-vault
  metadata used by the current device UI; or
- `MlsDeviceCredential`, which is recoverable, group-scoped MLS state.

The existing 32-byte device ID may be carried into the registration as the
installation identifier so the current device list and remote-revocation UX
remain stable. It is not treated as a public key.

## Invisible lifecycle

### First central-service use on an existing installation

1. The app loads the already-restored account identity from local encrypted
   state.
2. It creates a service-authentication key if one is not already present.
3. It requests a short-lived one-time registration challenge.
4. The account identity signs a versioned registration statement containing
   the account public key, installation ID, service-authentication public key,
   challenge, and issuance time.
5. The service verifies the identity signature and creates the installation
   binding.
6. The installation opens a session in the background.

No screen or user action is added to this flow.

### Normal launch and renewal

1. The installation asks for a short-lived one-time session challenge.
2. The service returns a random nonce tied to that registered installation.
3. The same installation signs the challenge with its local
   service-authentication private key.
4. The service consumes the challenge once and returns an opaque access token.
5. The client renews before expiry while it is online.

The access token can be short-lived because renewal is automatic. A missed
renewal caused by being offline is not a logout: the installation simply opens
a fresh session when connectivity returns.

### New installation or local key loss

A new installation restores the account exactly as it does today with the
noise ID and password, decrypts the account vault locally, creates its own
service-authentication key, and registers itself using the restored account
identity.

No existing installation is required. Losing only the transport key does not
lose the noise account.

### Remote revocation

Revocation:

- marks the installation binding revoked;
- revokes every active token for that installation;
- rejects new challenges and sessions for that binding; and
- maps to the existing client logout message.

Registration is not an automatic "unrevoke" operation. Reusing a revoked
installation ID or authentication key fails closed. A deliberate recovery or
new-installation registration must produce a new binding.

## Signed statements

Every signature uses a versioned, ASCII signing context before the binary
fields. Length-prefixed fields are used rather than delimiter-separated user
input.

The canonical signing encoder is implemented in
`crates/noise-core/src/central_auth.rs`:

- `field(value)` is a four-byte unsigned big-endian length followed by the raw
  value bytes;
- a record starts with `field(ASCII context)` followed by the four-byte
  unsigned big-endian protocol version;
- decoded 32-byte keys, IDs, and nonces are encoded with `field`;
- timestamps, registration versions, and revocation sequences are eight-byte
  unsigned big-endian integers; and
- JSON fields use canonical unpadded base64, but signatures cover the decoded
  binary values rather than the base64 text.

### Registration statement

Context:

`noise.central.device-registration.v1`

Fields:

1. account identity public key;
2. installation ID;
3. service-authentication public key;
4. server challenge ID;
5. server challenge nonce;
6. issued-at milliseconds; and
7. registration version.

The account identity key signs this statement. The service verifies the
challenge is unused, unexpired, and issued for registration before accepting
it.

### Session proof

Context:

`noise.central.session-proof.v1`

Fields:

1. account identity public key;
2. installation ID;
3. service-authentication public key;
4. server challenge ID;
5. server challenge nonce; and
6. issued-at milliseconds.

The service-authentication key on the same installation signs this statement.
The challenge is consumed atomically with session creation.

### Revocation statement

Context:

`noise.central.device-revocation.v1`

Fields:

1. account identity public key;
2. target installation ID;
3. target service-authentication public key;
4. revocation sequence; and
5. issued-at milliseconds.

The account identity key signs revocation. The sequence must advance so a
replayed older record cannot undo newer device state.

JSON serialization is not the signing format.

### Fixed version-one vectors

`noise-core` fixes cross-platform vectors using:

- account identity secret: 32 bytes of `0x11`;
- installation authentication secret: 32 bytes of `0x22`;
- installation ID: 32 bytes of `0x33`;
- challenge ID: 32 bytes of `0x44`;
- challenge nonce: 32 bytes of `0x55`;
- registration time/version: `1722000000123` / `7`;
- session-proof time: `1722000000456`; and
- revocation sequence/time: `8` / `1722000000789`.

| Statement | BLAKE3 of canonical signing bytes | Unpadded base64 Ed25519 signature |
| --- | --- | --- |
| registration | `4f06d990b71faa511e4abc4d062a10a323148f685dab923ee943aeefde7d08b1` | `xspjZYyfF4MM6vjXQA33cSL8Amb+piGSX+PTfd26fURgtR20VzVxWSR0+JR+SBxczcAZOfUUvGZqbFvf2It+AA` |
| session proof | `15ad48701d159c6e3007ab18f2dabb811fa3eb2484d5ad238bf25cbb1595ba09` | `9O0HYuRX5O9fzD1lAAXu96Aa0dHNQ59tr8Lv1iRdisIJ6LfnrHDY9P16h4xxxpbnspT6vU4B9pwmc5+hizFgCw` |
| revocation | `f9587a59f9c520838912c6604494ad28d83ac87ece27a14bc2759de1c8ce774a` | `FICi1Ej84OpSd/IjQcwM8qIZRzKqTF2srXUTD55shQFyoHErzIBxSwbIA5ntr1i8N9SoiLx5X7e+GXt0dmCUAw` |

## Service endpoints

The initial API contract is:

| Endpoint | Authentication | Purpose |
| --- | --- | --- |
| `POST /v1/auth/challenges/registration` | none; tightly rate-limited | Issue a registration challenge for an account key |
| `POST /v1/devices/register` | identity-signed challenge | Create an installation binding |
| `POST /v1/auth/challenges/session` | installation identifier; rate-limited | Issue a session challenge |
| `POST /v1/auth/sessions` | installation-signed challenge | Return an opaque access token |
| `DELETE /v1/auth/sessions/current` | current session | Revoke the current token |
| `POST /v1/devices/{installation_id}/revoke` | account-signed statement | Revoke a registered installation and its tokens |

Challenge responses are deliberately indistinguishable for unknown, blocked,
and deleted accounts where practical. Rate limits apply at the edge and by
pseudonymous account/installation identifiers.

## Token and challenge rules

- Challenge nonces are at least 32 random bytes.
- Only a hash of the challenge nonce is stored.
- A challenge expires after two minutes and is consumed exactly once.
- Access tokens contain at least 32 random bytes.
- Only a keyed hash of each access token is stored.
- Access tokens are returned once and never logged.
- The initial access-token lifetime is one hour.
- Clients renew automatically before expiry; the lifetime is not user-facing.
- Tokens are bound to one account and one registered installation.
- Passwords, vault keys, identity secrets, request bodies, raw tokens, and raw
  challenge nonces are never written to application logs.
- IP addresses and user agents are not persisted in the session tables.

Token hashing must use a server-side secret in addition to the random token so
a database-only compromise does not provide an offline token oracle.

## Failure behavior

| Condition | Client behavior |
| --- | --- |
| expired access token while online | renew silently and retry once |
| expired challenge | request a new challenge silently |
| offline at token expiry | keep local state usable; reconnect later |
| transient service failure | show the existing connection state; do not sign out |
| revoked installation | clear service credentials and show the existing remote-logout message |
| blocked account | stop service access using the safety restriction UX |
| invalid local authentication key | recover on this installation through the normal noise ID/password flow |

Retrying is only for transient transport and expiry races. It is not a way to
paper over invalid state: registration, challenge consumption, session
creation, and revocation are transactional and idempotent where applicable.

## Privacy boundary

The service learns the pseudonymous account public key, installation ID,
service-authentication public key, registration/revocation state, and session
timing. It does not need a device name or platform string. Those remain inside
the encrypted account vault for the user-facing device list.

The service must not derive account identity from an IP address, browser
fingerprint, email address, phone number, or hardware identifier.
