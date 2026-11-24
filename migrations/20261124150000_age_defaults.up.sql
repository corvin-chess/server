-- CV-140: age-appropriate messaging defaults (minors cannot DM by default).
-- Missing birth_year stays restrictive (both flags false).

ALTER TABLE users
    ADD COLUMN dm_enabled BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN public_chat_enabled BOOLEAN NOT NULL DEFAULT false;

-- Adults (approx age >= 18 from birth_year) get open messaging defaults.
UPDATE users
SET
    dm_enabled = true,
    public_chat_enabled = true
WHERE birth_year IS NOT NULL
  AND (EXTRACT(YEAR FROM now())::INT - birth_year) >= 18;
