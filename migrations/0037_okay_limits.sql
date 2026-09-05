-- Third free-text field alongside hard_limits/soft_limits, on both
-- profile tables — the "Limits & boundaries" redesign folds the old
-- structured item-by-item grid into three symmetric free-text fields
-- per person (hard/soft/okay) instead of two free-text fields plus a
-- separate grid.
ALTER TABLE keyholder_profiles ADD COLUMN okay_limits TEXT;
ALTER TABLE submissive_profiles ADD COLUMN okay_limits TEXT;
