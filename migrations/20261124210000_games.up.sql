-- CV-169: durable finished games + append-only events (ADR-004 / SECURITY_PRIVACY).

CREATE TABLE games (
    id TEXT PRIMARY KEY,
    initial_fen TEXT NOT NULL,
    white_id TEXT,
    black_id TEXT,
    white_username TEXT NOT NULL,
    black_username TEXT NOT NULL,
    time_class TEXT NOT NULL,
    rated BOOLEAN NOT NULL DEFAULT false,
    initial_ms BIGINT NOT NULL,
    increment_ms BIGINT NOT NULL,
    result TEXT NOT NULL,
    reason TEXT NOT NULL,
    final_fen TEXT NOT NULL,
    created_ms BIGINT NOT NULL,
    ended_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT games_id_len CHECK (char_length(id) BETWEEN 1 AND 32),
    CONSTRAINT games_initial_fen_len CHECK (char_length(initial_fen) BETWEEN 1 AND 128),
    CONSTRAINT games_final_fen_len CHECK (char_length(final_fen) BETWEEN 1 AND 128),
    CONSTRAINT games_username_white_len CHECK (char_length(white_username) BETWEEN 1 AND 64),
    CONSTRAINT games_username_black_len CHECK (char_length(black_username) BETWEEN 1 AND 64),
    CONSTRAINT games_time_class_ok CHECK (
        time_class IN ('bullet', 'blitz', 'rapid', 'classical', 'correspondence')
    ),
    CONSTRAINT games_initial_ms_nonneg CHECK (initial_ms >= 0),
    CONSTRAINT games_increment_ms_nonneg CHECK (increment_ms >= 0),
    CONSTRAINT games_result_ok CHECK (result IN ('1-0', '0-1', '1/2-1/2', '*')),
    CONSTRAINT games_reason_len CHECK (char_length(reason) BETWEEN 1 AND 64),
    CONSTRAINT games_created_ms_nonneg CHECK (created_ms >= 0)
);

CREATE INDEX games_white_id_idx ON games (white_id);
CREATE INDEX games_black_id_idx ON games (black_id);
CREATE INDEX games_ended_at_idx ON games (ended_at DESC);

CREATE TABLE game_events (
    game_id TEXT NOT NULL REFERENCES games (id) ON DELETE CASCADE,
    seq BIGINT NOT NULL,
    payload JSONB NOT NULL,
    PRIMARY KEY (game_id, seq),
    CONSTRAINT game_events_seq_nonneg CHECK (seq >= 0)
);

CREATE INDEX game_events_game_id_idx ON game_events (game_id);
