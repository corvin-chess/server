-- CV-131: durable account + opaque session tables (SECURITY_PRIVACY inventory).
-- Passkeys / OTP secrets land in CV-133; this migration only shapes identity storage.

CREATE TABLE users (
    id CHAR(26) PRIMARY KEY,
    username TEXT NOT NULL,
    username_lower TEXT NOT NULL,
    email TEXT,
    email_lower TEXT,
    birth_year SMALLINT,
    locale TEXT NOT NULL DEFAULT 'en',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    CONSTRAINT users_id_ulid_format CHECK (id ~ '^[0-9A-HJKMNP-TV-Z]{26}$'),
    CONSTRAINT users_username_nonempty CHECK (char_length(username) BETWEEN 1 AND 32),
    CONSTRAINT users_username_lower_matches CHECK (username_lower = lower(username)),
    CONSTRAINT users_email_lower_matches CHECK (
        (email IS NULL AND email_lower IS NULL)
        OR (email IS NOT NULL AND email_lower = lower(email))
    ),
    CONSTRAINT users_birth_year_range CHECK (
        birth_year IS NULL OR (birth_year BETWEEN 1900 AND 2100)
    ),
    CONSTRAINT users_locale_nonempty CHECK (char_length(locale) BETWEEN 2 AND 16)
);

CREATE UNIQUE INDEX users_username_lower_active_uidx
    ON users (username_lower)
    WHERE deleted_at IS NULL;

CREATE UNIQUE INDEX users_email_lower_active_uidx
    ON users (email_lower)
    WHERE email_lower IS NOT NULL AND deleted_at IS NULL;

CREATE TABLE sessions (
    id BYTEA PRIMARY KEY,
    user_id CHAR(26) NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    csrf_secret BYTEA NOT NULL,
    user_agent TEXT,
    ip INET,
    revoked_at TIMESTAMPTZ,
    CONSTRAINT sessions_id_len CHECK (octet_length(id) = 32),
    CONSTRAINT sessions_csrf_secret_len CHECK (octet_length(csrf_secret) = 32),
    CONSTRAINT sessions_expires_after_created CHECK (expires_at > created_at)
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);
