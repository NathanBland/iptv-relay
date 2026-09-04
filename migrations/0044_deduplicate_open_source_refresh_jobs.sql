-- Keep one open refresh job for each source before the unique index exists.
WITH ranked_jobs AS (
    SELECT id,
           row_number() OVER (
               PARTITION BY payload->>'sourceId'
               ORDER BY CASE status WHEN 'running' THEN 0 ELSE 1 END,
                        created_at ASC,
                        id ASC
           ) AS position
    FROM jobs
    WHERE kind = 'refresh-source'
      AND status IN ('queued', 'running')
)
UPDATE jobs
SET status = 'cancelled',
    completed_at = now(),
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    updated_at = now(),
    last_error = 'duplicate open source refresh removed by migration'
FROM ranked_jobs
WHERE jobs.id = ranked_jobs.id
  AND ranked_jobs.position > 1;
