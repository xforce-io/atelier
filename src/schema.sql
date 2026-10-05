PRAGMA user_version = 23;
CREATE TABLE workspace (id TEXT PRIMARY KEY, self_id TEXT NOT NULL);
CREATE TABLE workers (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE teams (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams(id),
    state TEXT NOT NULL CHECK(state IN ('pending','active','closed')),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    sender TEXT REFERENCES workers(id),
    source TEXT NOT NULL CHECK(source IN ('worker','core')),
    recipient TEXT NOT NULL REFERENCES workers(id),
    task_revision INTEGER NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL,
    reply_to TEXT REFERENCES messages(id),
    causation_id TEXT NOT NULL,
    event_key TEXT UNIQUE,
    CHECK(source = 'core' OR sender IS NOT NULL)
);
CREATE TABLE deliveries (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL UNIQUE REFERENCES messages(id),
    receiver TEXT NOT NULL REFERENCES workers(id),
    status TEXT NOT NULL CHECK(status IN ('queued','claimed','handled','blocked','uncertain','cancelled')),
    revision INTEGER NOT NULL DEFAULT 1,
    reason TEXT,
    run_id TEXT REFERENCES runs(id),
    claim_epoch TEXT,
    attempts INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX mailbox_order ON deliveries(receiver,status);
CREATE INDEX task_messages ON messages(task_id);
CREATE TABLE runtime (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    epoch TEXT NOT NULL,
    pid INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('running','stopped','blocked_unknown')),
    stop_requested INTEGER NOT NULL DEFAULT 0 CHECK(stop_requested IN (0,1))
);
CREATE TABLE requests (
    actor TEXT NOT NULL REFERENCES workers(id),
    id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    command TEXT NOT NULL CHECK(json_valid(command)),
    result TEXT NOT NULL CHECK(json_valid(result)),
    PRIMARY KEY(actor,id)
);

CREATE TABLE inputs (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE profiles (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));

CREATE TABLE decisions (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE INDEX decisions_by_task ON decisions(task_id);

CREATE TABLE artifact_submissions (run_id TEXT PRIMARY KEY REFERENCES runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE candidates (run_id TEXT PRIMARY KEY REFERENCES runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE artifacts (id TEXT PRIMARY KEY, run_id TEXT NOT NULL UNIQUE REFERENCES runs(id), task_id TEXT NOT NULL REFERENCES tasks(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE INDEX artifacts_by_task ON artifacts(task_id);
CREATE TABLE handoffs (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), artifact_id TEXT NOT NULL REFERENCES artifacts(id), receiver TEXT NOT NULL REFERENCES workers(id), state TEXT NOT NULL CHECK(state IN ('offered','accepted','rejected','superseded')), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE UNIQUE INDEX one_live_handoff ON handoffs(task_id,artifact_id,receiver) WHERE state NOT IN ('rejected','superseded');
CREATE TABLE checks (id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), state TEXT NOT NULL CHECK(state IN ('prepared','running','unknown','finished')), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE check_inputs (id TEXT PRIMARY KEY REFERENCES checks(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE UNIQUE INDEX one_active_check ON checks(run_id) WHERE state!='finished';
CREATE TABLE verifications (id TEXT PRIMARY KEY, run_id TEXT NOT NULL UNIQUE REFERENCES runs(id), task_id TEXT NOT NULL REFERENCES tasks(id), artifact_id TEXT NOT NULL REFERENCES artifacts(id), data TEXT NOT NULL CHECK(json_valid(data)));

CREATE TABLE connections (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE connection_tests (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE credentials (version_id TEXT PRIMARY KEY REFERENCES connection_versions(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE credential_requests (request_id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE connection_versions (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE execution_configs (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE execution_contexts (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE api_launches (run_id TEXT PRIMARY KEY REFERENCES runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE cli_resources (run_id TEXT PRIMARY KEY REFERENCES runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE cli_environments (id TEXT PRIMARY KEY REFERENCES execution_configs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE cli_logins (id TEXT PRIMARY KEY, data TEXT NOT NULL CHECK(json_valid(data)));

CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    worker_id TEXT NOT NULL REFERENCES workers(id),
    delivery_id TEXT NOT NULL REFERENCES deliveries(id),
    state TEXT NOT NULL CHECK(state IN ('prepared','running','stopped','unknown')),
    data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE UNIQUE INDEX workspace_single_run ON runs((1)) WHERE state IN ('prepared','running','unknown');

CREATE TABLE member_requests (
    delivery_id TEXT NOT NULL REFERENCES deliveries(id),
    operation_id TEXT NOT NULL,
    originating_run_id TEXT NOT NULL REFERENCES runs(id),
    tool_call_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    result TEXT NOT NULL CHECK(json_valid(result)),
    PRIMARY KEY(delivery_id,operation_id),
    UNIQUE(originating_run_id,tool_call_id)
);

CREATE TABLE message_dispositions (
    delivery_id TEXT PRIMARY KEY REFERENCES deliveries(id),
    run_id TEXT NOT NULL REFERENCES runs(id),
    task_revision INTEGER NOT NULL,
    data TEXT NOT NULL CHECK(json_valid(data))
);

CREATE TABLE reworks (id TEXT PRIMARY KEY REFERENCES deliveries(id), task_id TEXT NOT NULL REFERENCES tasks(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE INDEX reworks_by_task ON reworks(task_id);

CREATE TABLE blockers (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), run_id TEXT NOT NULL UNIQUE REFERENCES runs(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE INDEX blockers_by_task ON blockers(task_id);

CREATE TABLE acceptance_decisions (id TEXT PRIMARY KEY REFERENCES decisions(id), task_id TEXT NOT NULL REFERENCES tasks(id), data TEXT NOT NULL CHECK(json_valid(data)));
CREATE UNIQUE INDEX one_open_acceptance ON decisions(task_id) WHERE json_extract(data,'$.kind')='acceptance' AND json_extract(data,'$.state')='open';

CREATE UNIQUE INDEX recovery_event ON decisions(task_id,json_extract(data,'$.recovery.delivery_id'),json_extract(data,'$.recovery.event')) WHERE json_extract(data,'$.kind')='recovery';
