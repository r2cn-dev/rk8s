-- Add migration script here

CREATE TABLE repos (
    id UUID PRIMARY KEY NOT NULL DEFAULT uuid_generate_v4(),
    github_id BIGINT NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,

    name VARCHAR(255) NOT NULL UNIQUE,

    is_public BOOLEAN NOT NULL DEFAULT FALSE,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_repos_name ON repos (name);

DROP TRIGGER IF EXISTS set_timestamp ON repos;
CREATE TRIGGER set_timestamp
    BEFORE UPDATE ON repos
    FOR EACH ROW
EXECUTE PROCEDURE trigger_set_timestamp();