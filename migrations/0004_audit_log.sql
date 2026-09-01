CREATE TABLE audit_log (
    id TEXT PRIMARY KEY,
    actor_user_id TEXT REFERENCES users(id),
    link_id TEXT REFERENCES keyholder_submissive_links(id),
    action TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    detail TEXT
);

CREATE INDEX idx_audit_log_link_id ON audit_log(link_id);
CREATE INDEX idx_audit_log_entity ON audit_log(entity_type, entity_id);
CREATE INDEX idx_audit_log_occurred_at ON audit_log(occurred_at);
