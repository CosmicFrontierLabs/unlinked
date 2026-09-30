CREATE TABLE simulation_runs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    file_version_id UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    requested_by UUID REFERENCES users(id) ON DELETE SET NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed', 'cancelled')),
    request JSONB NOT NULL,
    trace JSONB,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ,
    CHECK ((status = 'running') = (finished_at IS NULL)),
    CHECK ((status = 'completed') = (trace IS NOT NULL))
);
CREATE INDEX simulation_runs_version_created_idx ON simulation_runs(file_version_id, created_at DESC);
CREATE INDEX simulation_runs_requested_by_idx ON simulation_runs(requested_by);
