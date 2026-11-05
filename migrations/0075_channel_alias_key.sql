-- Alias match key for channel output names.
--
-- normalize_channel_name strips only two-letter prefixes. Provider names such
-- as 'USA: NHL NETWORK [1080p]' keep their prefix, so aliases never match.
-- This function strips a prefix of two to four letters first. It does not
-- change normalize_channel_name, because EPG expression indexes use it.

CREATE OR REPLACE FUNCTION normalize_channel_alias_key(raw text)
RETURNS text
LANGUAGE plpgsql
IMMUTABLE
AS $$
BEGIN
    IF raw IS NULL THEN
        RETURN '';
    END IF;

    RETURN normalize_channel_name(
        regexp_replace(raw, '^[A-Za-z]{2,4}\s*[:|]\s*', '')
    );
END;
$$;
