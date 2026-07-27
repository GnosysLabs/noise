# Cloudflare production resources

## Encrypted media bucket

The production encrypted-media bucket is `noise-media-production`.

Required invariants:

- private R2 bucket;
- Standard default storage class;
- no `r2.dev` public URL;
- no public custom domain;
- browser CORS limited to `https://app.makenoise.chat`;
- browser methods limited to `GET`, `HEAD`, and `PUT`;
- completed encrypted media is not subject to an automatic expiration rule;
- completed media uses opaque object keys outside `temporary/`; and
- incomplete or unfinalized objects use `temporary/` and expire after one day.

Apply and verify the CORS policy:

```sh
npx wrangler r2 bucket cors set noise-media-production \
  --file deploy/cloudflare/noise-media-production-cors.json
npx wrangler r2 bucket cors list noise-media-production
```

Create and verify the temporary-upload lifecycle rule:

```sh
npx wrangler r2 bucket lifecycle add \
  noise-media-production \
  expire-temporary-uploads \
  temporary/ \
  --expire-days 1 \
  --abort-multipart-days 1
npx wrangler r2 bucket lifecycle list noise-media-production
```

Do not store Cloudflare OAuth tokens, R2 access-key IDs, R2 secret access keys,
presigned URLs, or account-specific endpoint credentials in Git.
