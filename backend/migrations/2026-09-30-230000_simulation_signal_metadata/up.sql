ALTER TABLE simulation_runs ADD COLUMN signals JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(signals) = 'array');
