# Noise media v2

Media v2 is the only media byte format produced or fetched by current Noise
clients. The event model still carries a `MediaAttachment`, but every storage
manifest produced for an attachment has `v = 2`.

## File layout

Video is converted to a network-optimized MP4 before encryption. The MP4 movie
metadata must be at the front of the file; a client rejects an outgoing video
when it cannot guarantee that layout.

Video and audio use these plaintext block boundaries:

- the first 2 MiB: independent 256 KiB bootstrap blocks;
- the remainder: independent blocks of at most 1 MiB.

Images and other files use blocks of at most 1 MiB. Every block is independently
encrypted, addressed, erasure-coded, and cached. This lets a player receive the
small front-of-file metadata without waiting for a full 1 MiB encrypted object,
while keeping attachment manifests bounded for large files.

The message event includes a small embedded poster. A visible video may prewarm
its first bootstrap block, but it does not download the complete video until
playback requests later ranges.

## Encrypted block envelope

The bytes erasure-coded into storage shards are:

```text
"NSB2"                  4 bytes
group_id_length         unsigned 16-bit big-endian
group_id                group_id_length UTF-8 bytes
XChaCha20 nonce         24 bytes
ciphertext              remaining bytes, including the AEAD tag
```

The manifest's object ID remains the authenticated Noise blob ID derived from
the group ID, nonce, and ciphertext. Reconstruction must verify that ID before
decryption.

## Relay transport

Media v2 uses `/v4/shards`.

`POST /v4/shards` accepts:

```text
"NSS2"                  4 bytes
shard_id                64 lowercase hexadecimal bytes
payload_hash            64 lowercase hexadecimal bytes
delete_token_hash       64 lowercase hexadecimal bytes
payload                 remaining raw shard bytes
```

`GET /v4/shards/{shard_id}` returns the raw shard payload as
`application/octet-stream`. The client verifies its exact length and BLAKE3
hash against the signed storage manifest before reconstruction.

`DELETE /v4/shards/{shard_id}` accepts the raw 32-byte deletion token.

These requests use the same OHTTP masking path as other private relay traffic.
There is no JSON or base64 layer around media payload bytes.

## Playback

Desktop custom-protocol responses and the iOS local HTTP proxy deliver at most
one 256 KiB bootstrap range at a time. Later AVPlayer/WebKit range requests are
mapped onto the attachment's encrypted blocks and downloaded concurrently where
a requested range crosses block boundaries.

Leaving a conversation deprioritizes or cancels queued media work. Decrypted
blocks remain in the private device cache so revisiting a conversation does not
show loading UI for bytes already fetched.

## Alpha reset

Clients do not contain a v1 media reader or fallback request path. Existing v1
attachments must be re-uploaded.
