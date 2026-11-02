-- Schema 13: the fit score beside the pinned health index.
-- `fit_score` is the weighted total; `fit_json` is the per-parameter
-- breakdown (points, weight, spec version). Both are nullable — a row
-- that has never been re-scored carries NULL, never a lie.

ALTER TABLE repos ADD COLUMN fit_score REAL;
ALTER TABLE repos ADD COLUMN fit_json TEXT;
