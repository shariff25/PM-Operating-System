-- pmos-bots: a PM's own working preferences (their level), and a task's own level, which
-- only the evals client sets (docs/staff-principal-spec.md §1).

CREATE TABLE bots_member_settings (
    handle     text PRIMARY KEY,
    level      text NOT NULL CHECK (level IN ('senior', 'staff', 'principal')),
    updated_at timestamptz NOT NULL
);

ALTER TABLE bots_tasks
    ADD COLUMN level text CHECK (level IN ('senior', 'staff', 'principal'));
