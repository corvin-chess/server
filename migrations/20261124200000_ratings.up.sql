-- CV-168: Glicko-2 per time class + once-only finish audit (ADR-007 / SECURITY_PRIVACY).

CREATE TABLE ratings (
    user_id CHAR(26) NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    time_class TEXT NOT NULL,
    rating DOUBLE PRECISION NOT NULL,
    rd DOUBLE PRECISION NOT NULL,
    volatility DOUBLE PRECISION NOT NULL,
    games INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, time_class),
    CONSTRAINT ratings_time_class_ok CHECK (
        time_class IN ('bullet', 'blitz', 'rapid', 'classical', 'correspondence')
    ),
    CONSTRAINT ratings_rating_finite CHECK (rating = rating),
    CONSTRAINT ratings_rd_positive CHECK (rd > 0),
    CONSTRAINT ratings_volatility_positive CHECK (volatility > 0),
    CONSTRAINT ratings_games_nonneg CHECK (games >= 0)
);

CREATE INDEX ratings_time_class_rating_idx ON ratings (time_class, rating DESC);

CREATE TABLE rating_updates (
    game_id TEXT PRIMARY KEY,
    time_class TEXT NOT NULL,
    white_id CHAR(26) NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    black_id CHAR(26) NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    result TEXT NOT NULL,
    reason TEXT NOT NULL,
    white_rating_before DOUBLE PRECISION NOT NULL,
    white_rd_before DOUBLE PRECISION NOT NULL,
    white_vol_before DOUBLE PRECISION NOT NULL,
    white_rating_after DOUBLE PRECISION NOT NULL,
    white_rd_after DOUBLE PRECISION NOT NULL,
    white_vol_after DOUBLE PRECISION NOT NULL,
    black_rating_before DOUBLE PRECISION NOT NULL,
    black_rd_before DOUBLE PRECISION NOT NULL,
    black_vol_before DOUBLE PRECISION NOT NULL,
    black_rating_after DOUBLE PRECISION NOT NULL,
    black_rd_after DOUBLE PRECISION NOT NULL,
    black_vol_after DOUBLE PRECISION NOT NULL,
    white_diff INTEGER NOT NULL,
    black_diff INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT rating_updates_game_id_len CHECK (char_length(game_id) BETWEEN 1 AND 64),
    CONSTRAINT rating_updates_time_class_ok CHECK (
        time_class IN ('bullet', 'blitz', 'rapid', 'classical', 'correspondence')
    ),
    CONSTRAINT rating_updates_result_ok CHECK (result IN ('1-0', '0-1', '1/2-1/2')),
    CONSTRAINT rating_updates_reason_nonempty CHECK (char_length(reason) BETWEEN 1 AND 64)
);

CREATE INDEX rating_updates_white_id_idx ON rating_updates (white_id);
CREATE INDEX rating_updates_black_id_idx ON rating_updates (black_id);
