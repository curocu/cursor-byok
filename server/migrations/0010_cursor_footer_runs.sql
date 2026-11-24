CREATE TABLE cursor_footer_runs (
    request_id TEXT PRIMARY KEY,
    parent_request_id TEXT
);
CREATE INDEX cursor_footer_runs_parent ON cursor_footer_runs(parent_request_id);
