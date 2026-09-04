CREATE OR REPLACE FUNCTION notify_job_inserted()
RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('job_available', NEW.id::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_notify_job_inserted ON jobs;
CREATE TRIGGER trg_notify_job_inserted
    AFTER INSERT ON jobs
    FOR EACH ROW
    EXECUTE FUNCTION notify_job_inserted();
