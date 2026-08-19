//! Canonical catalog reconciliation and read queries.
//!
//! Reconciliation converts activated provider streams into canonical channels
//! and binds those channels to active EPG channels. Read queries back the
//! control API with paginated, real data.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::PersistenceError;

const DEFAULT_PAGE_SIZE: i64 = 100;
const MAX_PAGE_SIZE: i64 = 500;

#[derive(Clone, Debug)]
pub struct CatalogRepository {
    pool: PgPool,
}

impl CatalogRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reconciles the active M3U snapshot for a provider account into canonical
    /// channels and channel-stream links. Streams that share a non-blank
    /// `tvg_id` merge into one channel; streams without a `tvg_id` become their
    /// own channel. Prior automatic channels for the account are replaced.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any reconcile query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn reconcile_provider_account(
        &self,
        account_id: Uuid,
    ) -> Result<ReconcileStats, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let channel_stats = upsert_canonical_channels(&mut transaction, account_id).await?;
        let link_count = relink_channel_streams(&mut transaction, account_id).await?;
        transaction.commit().await?;
        Ok(ReconcileStats {
            channels: channel_stats.channels,
            orphaned_channels_removed: channel_stats.orphans_removed,
            stream_links: link_count,
        })
    }

    /// Rebuilds EPG mappings for every automatic canonical channel that has a
    /// `tvg_id` matching an active EPG channel. Exact, case-insensitive
    /// `tvg-id` matches are applied automatically; ambiguous and fuzzy matches
    /// remain a future review flow.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any mapping query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn reconcile_epg_mappings(&self) -> Result<EpgMappingStats, PersistenceError> {
        let mut transaction = self.pool.begin().await?;
        let applied: i64 = sqlx::query(
            r"
            INSERT INTO channel_epg_mappings
                (channel_id, epg_channel_id, method, confidence, evidence, revision)
            SELECT c.id, ec.id, 'tvg-id', 0.99,
                   jsonb_build_object('match', 'tvg-id-exact'), 1
            FROM channels c
            JOIN epg_channels ec ON lower(ec.xmltv_id) = lower(c.canonical_key)
            JOIN source_snapshots ss
              ON ss.id = ec.source_snapshot_id AND ss.status = 'active'
            WHERE c.canonical_key IS NOT NULL
              AND c.canonical_key NOT LIKE 'stream:%'
            ON CONFLICT (channel_id) DO UPDATE SET
                epg_channel_id = EXCLUDED.epg_channel_id,
                method = EXCLUDED.method,
                confidence = EXCLUDED.confidence,
                evidence = EXCLUDED.evidence,
                revision = channel_epg_mappings.revision + 1,
                updated_at = now()
            ",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);

        let removed: i64 = sqlx::query(
            r"
            DELETE FROM channel_epg_mappings cem
            USING channels c
            WHERE cem.channel_id = c.id
              AND c.managed_by = 'automatic'
              AND c.canonical_key IS NOT NULL
              AND c.canonical_key NOT LIKE 'stream:%'
              AND NOT EXISTS (
                SELECT 1
                FROM epg_channels ec
                JOIN source_snapshots ss
                  ON ss.id = ec.source_snapshot_id AND ss.status = 'active'
                WHERE lower(ec.xmltv_id) = lower(c.canonical_key)
              )
            ",
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        .try_into()
        .unwrap_or(i64::MAX);
        transaction.commit().await?;
        Ok(EpgMappingStats {
            mappings_applied: applied,
            mappings_removed: removed,
        })
    }

    /// Lists canonical channels with server-side pagination and search.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_channels(
        &self,
        query: ChannelQuery,
    ) -> Result<ChannelPage, PersistenceError> {
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let offset = query.offset.unwrap_or(0).max(0);
        let pattern = query.search.as_deref().map(|value| {
            format!(
                "%{}%",
                value
                    .trim()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });

        let total: i64 = sqlx::query_scalar(
            r"
            SELECT count(*) FROM channels
            WHERE ($1::text IS NULL
                   OR lower(name) LIKE lower($1) ESCAPE '\'
                   OR lower(coalesce(group_name, '')) LIKE lower($1) ESCAPE '\')
              AND ($2::text IS NULL OR lower(coalesce(group_name, '')) = lower($2))
            ",
        )
        .bind(pattern.as_deref())
        .bind(
            query
                .group
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        )
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, ChannelRow>(
            r"
            SELECT
                c.id,
                c.channel_number,
                c.name,
                coalesce(c.group_name, 'Uncategorized') AS group_name,
                c.logo_url,
                c.enabled,
                count(cs.provider_stream_id) AS stream_count,
                EXISTS (SELECT 1 FROM channel_epg_mappings m WHERE m.channel_id = c.id) AS epg_mapped
            FROM channels c
            LEFT JOIN channel_streams cs ON cs.channel_id = c.id
            WHERE ($1::text IS NULL
                   OR lower(c.name) LIKE lower($1) ESCAPE '\'
                   OR lower(coalesce(c.group_name, '')) LIKE lower($1) ESCAPE '\')
              AND ($2::text IS NULL OR lower(coalesce(c.group_name, '')) = lower($2))
            GROUP BY c.id
            ORDER BY min(c.channel_number), c.id
            LIMIT $3 OFFSET $4
            ",
        )
        .bind(pattern.as_deref())
        .bind(query.group.as_deref().map(str::trim).filter(|value| !value.is_empty()))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(ChannelPage {
            total,
            limit,
            offset,
            items: rows,
        })
    }

    /// Lists programmes for mapped canonical channels with pagination.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when the query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn list_programmes(
        &self,
        query: ProgrammeQuery,
    ) -> Result<ProgrammePage, PersistenceError> {
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let offset = query.offset.unwrap_or(0).max(0);

        let total: i64 = sqlx::query_scalar(
            r"
            SELECT count(*)
            FROM programmes p
            JOIN epg_channels ec ON ec.id = p.epg_channel_id
            JOIN channel_epg_mappings m ON m.epg_channel_id = ec.id
            WHERE ($1::uuid IS NULL OR m.channel_id = $1)
              AND ($2::timestamptz IS NULL OR p.starts_at >= $2)
            ",
        )
        .bind(query.channel_id)
        .bind(query.from)
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, ProgrammeRow>(
            r"
            SELECT
                concat(m.channel_id::text, ':', p.id::text) AS id,
                c.name AS channel_name,
                p.title,
                p.subtitle,
                p.description,
                p.categories,
                p.starts_at,
                coalesce(p.stops_at, 'infinity'::timestamptz) AS stops_at,
                es.name AS source_name
            FROM programmes p
            JOIN epg_channels ec ON ec.id = p.epg_channel_id
            JOIN channel_epg_mappings m ON m.epg_channel_id = ec.id
            JOIN channels c ON c.id = m.channel_id
            LEFT JOIN epg_sources es ON es.id = ec.epg_source_id
            WHERE ($1::uuid IS NULL OR m.channel_id = $1)
              AND ($2::timestamptz IS NULL OR p.starts_at >= $2)
            ORDER BY p.starts_at DESC, p.id DESC
            LIMIT $3 OFFSET $4
            ",
        )
        .bind(query.channel_id)
        .bind(query.from)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(ProgrammePage {
            total,
            limit,
            offset,
            items: rows,
        })
    }

    /// Returns real system counts for the control API overview.
    ///
    /// # Errors
    ///
    /// Returns [`PersistenceError::Database`] when any count query fails.
    #[allow(clippy::cast_precision_loss, clippy::missing_errors_doc)]
    pub async fn system_counts(&self) -> Result<SystemCounts, PersistenceError> {
        let row = sqlx::query_as::<_, SystemCountRow>(
            r"
            SELECT
                count(*) FILTER (WHERE enabled) AS channel_count,
                count(*) FILTER (
                    WHERE enabled AND EXISTS (
                        SELECT 1 FROM channel_streams cs
                        JOIN provider_streams ps ON ps.id = cs.provider_stream_id
                        JOIN source_snapshots ss ON ss.id = ps.snapshot_id
                        WHERE cs.channel_id = channels.id AND ss.status = 'active'
                    )
                ) AS healthy_stream_count,
                count(*) FILTER (
                    WHERE enabled AND EXISTS (
                        SELECT 1 FROM channel_epg_mappings m WHERE m.channel_id = channels.id
                    )
                ) AS guide_mapped_count
            FROM channels
            ",
        )
        .fetch_one(&self.pool)
        .await?;

        let guide_coverage = if row.channel_count == 0 {
            0.0
        } else {
            (row.guide_mapped_count as f64 / row.channel_count as f64) * 100.0
        };

        Ok(SystemCounts {
            channels: row.channel_count,
            healthy_streams: row.healthy_stream_count,
            guide_coverage,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReconcileStats {
    pub channels: i64,
    pub orphaned_channels_removed: i64,
    pub stream_links: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct EpgMappingStats {
    pub mappings_applied: i64,
    pub mappings_removed: i64,
}

#[derive(Clone, Debug, Default)]
pub struct ChannelQuery {
    pub search: Option<String>,
    pub group: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct ProgrammeQuery {
    pub channel_id: Option<Uuid>,
    pub from: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ChannelRow {
    pub id: Uuid,
    pub channel_number: String,
    pub name: String,
    pub group_name: String,
    pub logo_url: Option<String>,
    pub enabled: bool,
    pub stream_count: i64,
    pub epg_mapped: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChannelPage {
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub items: Vec<ChannelRow>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct ProgrammeRow {
    pub id: String,
    pub channel_name: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Value,
    pub starts_at: DateTime<Utc>,
    pub stops_at: DateTime<Utc>,
    pub source_name: Option<String>,
}

impl ProgrammeRow {
    pub fn category_list(&self) -> Vec<String> {
        self.categories
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgrammePage {
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub items: Vec<ProgrammeRow>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SystemCounts {
    pub channels: i64,
    pub healthy_streams: i64,
    pub guide_coverage: f64,
}

#[derive(Debug, FromRow)]
#[allow(clippy::struct_field_names)]
struct SystemCountRow {
    channel_count: i64,
    healthy_stream_count: i64,
    guide_mapped_count: i64,
}

#[derive(Debug, FromRow)]
struct ReconcileCountRow {
    channels: i64,
    orphans_removed: i64,
}

#[allow(clippy::too_many_lines)]
async fn upsert_canonical_channels(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<ReconcileCountRow, PersistenceError> {
    let inserted: i64 = sqlx::query(
        r"
        WITH active_snapshot AS (
            SELECT id
            FROM source_snapshots
            WHERE provider_account_id = $1 AND kind = 'm3u' AND status = 'active'
            ORDER BY activated_at DESC NULLS LAST
            LIMIT 1
        ),
        grouped AS (
            SELECT
                COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key) AS canonical_key,
                (array_agg(ps.name ORDER BY ps.id))[1] AS name,
                (array_agg(ps.group_name ORDER BY ps.id)
                    FILTER (WHERE ps.group_name IS NOT NULL))[1] AS group_name,
                (array_agg(ps.logo_url ORDER BY ps.id)
                    FILTER (WHERE ps.logo_url IS NOT NULL AND ps.logo_url <> ''))[1] AS logo_url,
                (array_agg(ps.channel_number ORDER BY ps.id)
                    FILTER (WHERE ps.channel_number IS NOT NULL AND ps.channel_number <> ''))[1] AS preferred_number
            FROM provider_streams ps
            WHERE ps.snapshot_id = (SELECT id FROM active_snapshot) AND ps.supported
            GROUP BY canonical_key
        ),
        preferred_deduped AS (
            SELECT DISTINCT ON (preferred_number)
                canonical_key, name, group_name, logo_url, preferred_number
            FROM grouped
            WHERE preferred_number IS NOT NULL
            ORDER BY preferred_number, canonical_key
        ),
        all_groups AS (
            SELECT canonical_key, name, group_name, logo_url, preferred_number
            FROM preferred_deduped
            UNION ALL
            SELECT canonical_key, name, group_name, logo_url, NULL::text AS preferred_number
            FROM grouped
            WHERE preferred_number IS NULL
        ),
        channel_rows AS (
            SELECT
                regexp_replace(
                    md5('iptv-channel:v1:' || all_groups.canonical_key),
                    '^(.{8})(.{4})(.{4})(.{4})(.{12})$',
                    '\1-\2-\3-\4-\5'
                )::uuid AS id,
                all_groups.canonical_key,
                all_groups.name,
                all_groups.group_name,
                all_groups.logo_url,
                CASE
                    WHEN all_groups.preferred_number IS NOT NULL
                         AND NOT EXISTS (
                             SELECT 1 FROM channels c
                             WHERE c.channel_number = all_groups.preferred_number
                         )
                    THEN all_groups.preferred_number
                    ELSE nextval('canonical_channel_number_seq')::text
                END AS channel_number
            FROM all_groups
        )
        INSERT INTO channels
            (id, channel_number, name, group_name, logo_url, enabled,
             managed_by, provider_account_id, canonical_key, revision)
        SELECT id, channel_number, name, group_name, logo_url, true,
               'automatic', $1, canonical_key, 1
        FROM channel_rows
        ON CONFLICT (id) DO UPDATE SET
            name = EXCLUDED.name,
            group_name = EXCLUDED.group_name,
            logo_url = EXCLUDED.logo_url,
            enabled = true,
            updated_at = now(),
            revision = channels.revision + 1
        ",
    )
    .bind(account_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);

    let orphans_removed: i64 = sqlx::query(
        r"
        DELETE FROM channels
        WHERE provider_account_id = $1
          AND managed_by = 'automatic'
          AND canonical_key NOT IN (
              SELECT COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key)
              FROM provider_streams ps
              JOIN source_snapshots ss ON ss.id = ps.snapshot_id
              WHERE ss.provider_account_id = $1
                AND ss.status = 'active'
                AND ss.kind = 'm3u'
                AND ps.supported
          )
        ",
    )
    .bind(account_id)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
    .try_into()
    .unwrap_or(i64::MAX);

    Ok(ReconcileCountRow {
        channels: inserted,
        orphans_removed,
    })
}

async fn relink_channel_streams(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<i64, PersistenceError> {
    sqlx::query(
        r"
        DELETE FROM channel_streams cs
        USING channels c
        WHERE cs.channel_id = c.id
          AND c.provider_account_id = $1
          AND c.managed_by = 'automatic'
        ",
    )
    .bind(account_id)
    .execute(&mut **transaction)
    .await?;

    let result = sqlx::query(
        r"
        INSERT INTO channel_streams (channel_id, provider_stream_id, priority, evidence)
        SELECT
            c.id,
            ps.id,
            row_number() OVER (PARTITION BY c.id ORDER BY ps.id) - 1,
            jsonb_build_object('reconciled', now())
        FROM provider_streams ps
        JOIN source_snapshots ss ON ss.id = ps.snapshot_id
        JOIN channels c
          ON c.provider_account_id = $1
         AND c.canonical_key = COALESCE(NULLIF(ps.tvg_id, ''), 'stream:' || ps.stable_key)
        WHERE ss.provider_account_id = $1
          AND ss.status = 'active'
          AND ss.kind = 'm3u'
          AND ps.supported
        ON CONFLICT (channel_id, provider_stream_id) DO NOTHING
        ",
    )
    .bind(account_id)
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected().try_into().unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programme_row_extracts_categories_from_json_array() {
        let row = ProgrammeRow {
            id: "p1".to_owned(),
            channel_name: "News".to_owned(),
            title: "Bulletin".to_owned(),
            subtitle: None,
            description: None,
            categories: serde_json::json!(["News", "Politics"]),
            starts_at: Utc::now(),
            stops_at: Utc::now(),
            source_name: None,
        };
        assert_eq!(
            row.category_list(),
            vec!["News".to_owned(), "Politics".to_owned()]
        );
    }

    #[test]
    fn programme_row_tolerates_non_array_categories() {
        let row = ProgrammeRow {
            id: "p2".to_owned(),
            channel_name: "News".to_owned(),
            title: "Bulletin".to_owned(),
            subtitle: None,
            description: None,
            categories: serde_json::json!({}),
            starts_at: Utc::now(),
            stops_at: Utc::now(),
            source_name: None,
        };
        assert!(row.category_list().is_empty());
    }

    #[test]
    fn page_size_clamps_are_applied_by_callers() {
        assert_eq!(DEFAULT_PAGE_SIZE, 100);
        assert_eq!(MAX_PAGE_SIZE, 500);
    }
}
