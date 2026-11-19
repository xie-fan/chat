CREATE TABLE android_fcm_devices (
    session_id  uuid PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    token       text UNIQUE CHECK (char_length(token) BETWEEN 1 AND 1024),
    binding     uuid NOT NULL UNIQUE,
    key_hash    text NOT NULL,
    generation  bigint NOT NULL CHECK (generation > 0)
);
