ALTER TABLE users
    DROP COLUMN IF EXISTS public_chat_enabled,
    DROP COLUMN IF EXISTS dm_enabled;
