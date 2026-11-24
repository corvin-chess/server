-- CV-133: passkey credentials + email OTP challenges (SECURITY_PRIVACY inventory).
-- Detection scoring config never lives here.

CREATE TABLE passkeys (
    credential_id BYTEA PRIMARY KEY,
    user_id CHAR(26) NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    passkey_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    CONSTRAINT passkeys_credential_id_nonempty CHECK (octet_length(credential_id) BETWEEN 16 AND 1024)
);

CREATE INDEX passkeys_user_id_idx ON passkeys (user_id);

CREATE TABLE email_otps (
    id BYTEA PRIMARY KEY,
    email_lower TEXT NOT NULL,
    purpose TEXT NOT NULL,
    code_hash BYTEA NOT NULL,
    username TEXT,
    birth_year SMALLINT,
    locale TEXT NOT NULL DEFAULT 'en',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    attempts INT NOT NULL DEFAULT 0,
    CONSTRAINT email_otps_id_len CHECK (octet_length(id) = 16),
    CONSTRAINT email_otps_code_hash_len CHECK (octet_length(code_hash) = 32),
    CONSTRAINT email_otps_purpose_known CHECK (purpose IN ('register', 'login')),
    CONSTRAINT email_otps_expires_after_created CHECK (expires_at > created_at),
    CONSTRAINT email_otps_attempts_nonneg CHECK (attempts >= 0),
    CONSTRAINT email_otps_register_needs_username CHECK (
        purpose <> 'register' OR (username IS NOT NULL AND char_length(username) BETWEEN 1 AND 32)
    ),
    CONSTRAINT email_otps_birth_year_range CHECK (
        birth_year IS NULL OR (birth_year BETWEEN 1900 AND 2100)
    )
);

CREATE INDEX email_otps_email_lower_idx ON email_otps (email_lower);
CREATE INDEX email_otps_expires_at_idx ON email_otps (expires_at);
