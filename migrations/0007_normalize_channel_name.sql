-- Channel name normalization function for EPG matching.
--
-- Strips country prefixes, quality tokens, resolution markers, and
-- punctuation from channel names so that M3U and XMLTV names that refer
-- to the same channel can match.
--
-- Examples:
--   'UK: SKY SPORTS ACTION [H265] [720p]' -> 'sky sports action'
--   'UK| SKY SPORTS ACTION FHD'           -> 'sky sports action'
--   'RS: RTS 1 [1080p]'                   -> 'rts 1'
--   'ES| MTV SD'                          -> 'mtv'

CREATE OR REPLACE FUNCTION normalize_channel_name(raw text)
RETURNS text
LANGUAGE plpgsql
IMMUTABLE
AS $$
DECLARE
    stripped text;
BEGIN
    IF raw IS NULL THEN
        RETURN '';
    END IF;

    -- Remove leading country/region prefix before ':' or '|' delimiter.
    -- e.g. "UK: SKY..." or "ES| MTV..." becomes "SKY..."
    stripped := regexp_replace(raw, '^[A-Z]{2}\s*[:|]\s*', '');

    -- Remove quality and resolution tokens in brackets.
    -- e.g. "[1080p]", "[720p]", "[H265]"
    stripped := regexp_replace(stripped, '\[[^\]]*\]', ' ', 'g');

    -- Remove standalone quality tokens.
    -- e.g. "FHD", "HD", "SD", "H265", "H264", "HEVC", "LIVEEVENT"
    -- Uses \y (POSIX word boundary) instead of \b (backspace in POSIX).
    stripped := regexp_replace(
        stripped,
        '\y(FHD|HD|SD|H265|H264|HEVC|LIVEEVENT|UHD|4K)\y',
        ' ',
        'gi'
    );

    -- Replace all non-alphanumeric characters with spaces.
    stripped := regexp_replace(stripped, '[^A-Za-z0-9]+', ' ', 'g');

    -- Collapse whitespace, trim, and lowercase.
    stripped := lower(trim(regexp_replace(stripped, '\s+', ' ', 'g')));

    RETURN stripped;
END;
$$;
