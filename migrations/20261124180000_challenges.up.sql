-- CV-167: durable challenge rows for shareable challenge links (ADR-006).
-- Player ids are opaque seat tokens (not FK to users) so guest path stays open (CV-200).

CREATE TABLE challenges (
    id CHAR(26) PRIMARY KEY,
    from_player_id TEXT NOT NULL,
    opponent_player_id TEXT,
    link_token TEXT NOT NULL,
    time_class TEXT NOT NULL,
    initial_ms BIGINT NOT NULL,
    increment_ms BIGINT NOT NULL,
    rated BOOLEAN NOT NULL,
    color_pref TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    game_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT challenges_id_ulid_format CHECK (id ~ '^[0-9A-HJKMNP-TV-Z]{26}$'),
    CONSTRAINT challenges_from_len CHECK (char_length(from_player_id) BETWEEN 1 AND 64),
    CONSTRAINT challenges_opponent_len CHECK (
        opponent_player_id IS NULL
        OR char_length(opponent_player_id) BETWEEN 1 AND 64
    ),
    CONSTRAINT challenges_link_token_len CHECK (char_length(link_token) BETWEEN 16 AND 64),
    CONSTRAINT challenges_time_class_nonempty CHECK (char_length(time_class) BETWEEN 1 AND 32),
    CONSTRAINT challenges_initial_ms_range CHECK (initial_ms > 0 AND initial_ms <= 604800000),
    CONSTRAINT challenges_increment_ms_range CHECK (increment_ms >= 0 AND increment_ms <= 3600000),
    CONSTRAINT challenges_color_pref CHECK (color_pref IN ('random', 'white', 'black')),
    CONSTRAINT challenges_status CHECK (
        status IN ('open', 'accepted', 'declined', 'expired', 'cancelled')
    ),
    CONSTRAINT challenges_game_id_len CHECK (
        game_id IS NULL OR char_length(game_id) BETWEEN 1 AND 32
    ),
    CONSTRAINT challenges_expires_after_created CHECK (expires_at > created_at)
);

CREATE UNIQUE INDEX challenges_link_token_uidx ON challenges (link_token);
CREATE INDEX challenges_status_expires_idx ON challenges (status, expires_at);
CREATE INDEX challenges_from_player_idx ON challenges (from_player_id);
