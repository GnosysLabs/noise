BEGIN;

SET LOCAL search_path = noise, public;

-- Account vaults are complete encrypted snapshots. The API and watch paths
-- only expose the revision selected by account_vault_heads, so retaining every
-- superseded snapshot duplicates an account's full vault after each update.
CREATE OR REPLACE FUNCTION noise.prune_superseded_account_vault_versions()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, noise
AS $$
BEGIN
    DELETE FROM noise.account_vault_versions
    WHERE locator = NEW.locator
      AND revision <> NEW.revision;
    RETURN NEW;
END;
$$;

REVOKE ALL ON FUNCTION noise.prune_superseded_account_vault_versions()
    FROM PUBLIC;

DROP TRIGGER IF EXISTS account_vault_heads_prune_versions
    ON noise.account_vault_heads;

CREATE TRIGGER account_vault_heads_prune_versions
AFTER INSERT OR UPDATE OF revision ON noise.account_vault_heads
FOR EACH ROW
EXECUTE FUNCTION noise.prune_superseded_account_vault_versions();

-- Remove snapshots that were already superseded before this invariant was
-- installed. Every remaining row is reachable through account_vault_heads.
DELETE FROM noise.account_vault_versions AS versions
WHERE NOT EXISTS (
    SELECT 1
    FROM noise.account_vault_heads AS heads
    WHERE heads.locator = versions.locator
      AND heads.revision = versions.revision
);

INSERT INTO noise.schema_migrations (version, name)
VALUES (9, 'current_account_vault_only');

COMMIT;
