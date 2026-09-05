# Live job diagnostics

## Observation on September 5, 2026

Read-only API checks occurred at 00:38 UTC. The system reported gateway version `0.1.0`, zero published channels, and zero active viewers.

The recent job list contained 64 jobs: one running, three cancelled, 50 failed, and ten succeeded. No reconciliation child jobs appeared.

| Job ID | State | Evidence |
| --- | --- | --- |
| `01a06ef2-16ab-7600-af1a-f929b9d2ea28` | Running with no observed progress | Attempt 2 of 3; 55%; 1,136,956 records. |
| `01a06eee-7176-7b91-98cb-fade1aa696fc` | Cancelled | Completion at 00:22:31 UTC. |
| `01a06eeb-57d8-7b43-8b2a-3c24adb6074a` | Cancelled | Completion at 00:18:33 UTC. |
| `01a06ed8-5461-7353-b583-3ec9955d3293` | Cancelled | Completion at 00:15:10 UTC. |
| `01a06eb1-dfce-74e0-a654-400345f7bb0f` | Failed | Three attempts; 85%; source catalog reconciliation failed. |
| `01a06e8c-616b-71f0-a877-bdd3d52af519` | Failed | Three attempts; 85%; source catalog reconciliation failed. |

The running job update time advanced from 00:38:03 to 00:38:33 UTC. Its record count, byte count, stage, and percentage did not change.

The M3U source had no successful refresh time. The XMLTV source had a successful refresh at 21:40:19 UTC on September 4.

## Assessment

The running job is present. Its recent update time indicates activity, but does not prove useful work. The API does not expose its owner or heartbeat.

The deployment reports an older version than repository version `0.2.8`. Its progress format and absent child jobs match the older serial reconciliation path.

The evidence supports a deployment mismatch. The API error summary does not identify the database error. Database and worker logs are necessary for that diagnosis.

## Operator procedure

1. Record the deployed core and worker image digests.
2. Compare each digest with the approved release manifest.
3. Deploy the same validated release to the core and every worker.
4. Preserve the database and its migration history.
5. Check `/api/v1/system` for the expected gateway version and Git commit.
6. Check the next authorized refresh for reconciliation child jobs and durable progress.
7. If reconciliation fails again, inspect worker and PostgreSQL logs for the job ID.

Deployment, service restarts, refresh requests, and live database changes were outside this diagnostic check.
