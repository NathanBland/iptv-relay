# Parser fuzzing

The fuzz package is intentionally outside the production workspace and never
contains provider data. Install `cargo-fuzz`, then run one target at a time:

```bash
cargo +nightly fuzz run m3u -- -max_total_time=60
cargo +nightly fuzz run xmltv -- -max_total_time=60
cargo +nightly fuzz run xtream -- -max_total_time=60
cargo +nightly fuzz run events -- -max_total_time=60
```

Nightly runs should replace `60` with `1800`. Crash artifacts must be reviewed
and minimized before a sanitized regression fixture is added under `tests/fixtures`.
