CREATE TABLE background_task_runs (
    task_name TEXT PRIMARY KEY,
    last_run_at INTEGER NOT NULL,
    last_run_ok INTEGER NOT NULL,
    last_error TEXT,
    rows_processed INTEGER NOT NULL
);
