# Relay update channels

`stable.json` and `canary.json` are exact-byte signed release manifests. Their
detached Ed25519 signatures use the matching `.json.sig` filename. The relay
verifies the signature embedded in its binary before it parses or trusts any
manifest field.

Do not hand-edit a published manifest. Generate and sign it with
`scripts/promote-relay-release.sh`.
