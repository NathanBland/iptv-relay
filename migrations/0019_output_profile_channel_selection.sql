ALTER TABLE output_profiles
    ADD COLUMN IF NOT EXISTS include_all_channels boolean NOT NULL DEFAULT true;

CREATE INDEX IF NOT EXISTS output_profiles_previous_token_hash_idx
    ON output_profiles(previous_token_hash)
    WHERE enabled AND previous_token_hash IS NOT NULL;
