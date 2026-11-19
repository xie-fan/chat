CREATE TABLE android_fcm_devices (
    token       text PRIMARY KEY CHECK (char_length(token) BETWEEN 1 AND 1024),
    session_id  uuid NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    binding     uuid NOT NULL
);
