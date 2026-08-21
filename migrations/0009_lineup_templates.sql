-- Lineup templates define a provider lineup (e.g. DIRECTV, Sky UK, Freeview).
CREATE TABLE IF NOT EXISTS lineup_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    package_name TEXT NOT NULL,
    country TEXT NOT NULL DEFAULT 'US',
    description TEXT,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Lineup categories map to channel groups.
CREATE TABLE IF NOT EXISTS lineup_categories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL REFERENCES lineup_templates(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    sort_order INT NOT NULL DEFAULT 0,
    UNIQUE(template_id, name)
);

-- Lineup channels define the expected channels in a category.
CREATE TABLE IF NOT EXISTS lineup_channels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category_id UUID NOT NULL REFERENCES lineup_categories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    channel_number TEXT NOT NULL,
    aliases TEXT[] NOT NULL DEFAULT '{}',
    enabled BOOLEAN NOT NULL DEFAULT true,
    UNIQUE(category_id, channel_number)
);

CREATE INDEX IF NOT EXISTS lineup_categories_template_idx ON lineup_categories(template_id);
CREATE INDEX IF NOT EXISTS lineup_channels_category_idx ON lineup_channels(category_id);
