use atelier::{Error, database::Database, model::*, store::Store};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
    time::Duration,
};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    store: Store,
    human: String,
    team: Team,
}
impl Fixture {
    fn new(digital_leader: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::in_directory(digital_leader, dir)
    }
    fn in_directory(digital_leader: bool, dir: TempDir) -> Self {
        let initialized = Store::init(dir.path(), "测试本人").unwrap();
        let human = initialized["self"]["id"].as_str().unwrap().to_string();
        let mut store = Store::open(dir.path()).unwrap();
        let mut members = vec![human.clone()];
        for i in 0..3 {
            let worker = store
                .execute(
                    &format!("w{i}"),
                    &Command::WorkerCreate {
                        connection: None,
                        name: format!("成员 {i}"),
                        description: String::new(),
                    },
                )
                .unwrap();
            members.push(worker["id"].as_str().unwrap().to_string());
        }
        let team = Team {
            id: String::new(),
            name: "测试团队".into(),
            leader: members[usize::from(digital_leader)].clone(),
            executor: Some(members[2].clone()),
            verifier: Some(members[3].clone()),
            acceptor: human.clone(),
            members,
            grants: BTreeMap::new(),
            revision: 1,
            authorization_revision: 1,
        };
        let team = serde_json::from_value(
            store
                .execute("team", &Command::TeamCreate { team })
                .unwrap(),
        )
        .unwrap();
        Self {
            dir,
            store,
            human,
            team,
        }
    }
    fn create(&mut self) -> Value {
        self.store
            .execute(
                "task",
                &Command::TaskCreate {
                    team_id: self.team.id.clone(),
                    goal: "离线双人井字棋".into(),
                },
            )
            .unwrap()
    }
    fn complete_contract(&mut self, id: &str) -> Value {
        self.store
            .execute(
                "contract",
                &Command::TaskUpdate {
                    decision_id: None,
                    id: id.into(),
                    revision: 1,
                    goal: None,
                    contract: ContractPatch {
                        inputs: Some("固定测试输入".into()),
                        delivery: Some("离线网页".into()),
                        verification: Some("独立规则检验".into()),
                        ..Default::default()
                    },
                    refresh_team: false,
                },
            )
            .unwrap()
    }
}

#[test]
fn initialize_never_overwrites_existing_files_or_creates_on_read() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Store::open(dir.path()).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    std::fs::write(dir.path().join("keep"), "important").unwrap();
    assert!(Store::init(dir.path(), "human").is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("keep")).unwrap(),
        "important"
    );
}

#[test]
fn create_and_retry_survive_process_connection_lifetime() {
    let mut f = Fixture::new(true);
    let first = f.create();
    let task_id = first["task"]["id"].as_str().unwrap();
    drop(f.store);
    let mut store = Store::open(f.dir.path()).unwrap();
    assert_eq!(store.request("task").unwrap(), first);
    assert_eq!(
        store
            .execute(
                "task",
                &Command::TaskCreate {
                    team_id: f.team.id,
                    goal: "离线双人井字棋".into()
                }
            )
            .unwrap(),
        first
    );
    let queue = store.mailbox(Some(&f.team.leader)).unwrap();
    assert_eq!(queue.as_array().unwrap().len(), 1);
    assert_eq!(queue[0]["status"], "queued");
    assert_eq!(queue[0]["message"]["taskId"], task_id);
    assert_eq!(store.list("task").unwrap().as_array().unwrap().len(), 1);
}

#[test]
fn concurrent_connections_with_one_request_create_exactly_one_task() {
    let f = Fixture::new(false);
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let path = f.dir.path().to_path_buf();
            let team = f.team.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                store
                    .execute(
                        "concurrent",
                        &Command::TaskCreate {
                            team_id: team,
                            goal: "same".into(),
                        },
                    )
                    .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(results.iter().all(|r| r == &results[0]));
    assert_eq!(f.store.list("task").unwrap().as_array().unwrap().len(), 1);
}

#[test]
fn message_insert_failure_rolls_back_task_and_request() {
    let mut f = Fixture::new(false);
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_message BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(
        f.store
            .execute(
                "fault",
                &Command::TaskCreate {
                    team_id: f.team.id.clone(),
                    goal: "will fail".into()
                }
            )
            .is_err()
    );
    assert_eq!(f.store.list("task").unwrap(), json!([]));
    assert!(matches!(f.store.request("fault"), Err(Error::NotFound(_))));
    db.execute_batch("DROP TRIGGER fail_message;").unwrap();
    f.store
        .execute(
            "fault",
            &Command::TaskCreate {
                team_id: f.team.id,
                goal: "will fail".into(),
            },
        )
        .unwrap();
}

#[test]
fn administrator_cannot_intake_for_a_digital_leader() {
    let mut f = Fixture::new(true);
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    let result = f.store.execute(
        "impersonate",
        &Command::Intake {
            id: id.into(),
            revision: 1,
            decision: IntakeDecision::Wait,
            reason: "管理身份".into(),
        },
    );
    assert!(matches!(result, Err(Error::Forbidden(_))));
    assert_eq!(f.store.task(id).unwrap().revision, 1);
    let delivery = created["delivery"]["deliveryId"].as_str().unwrap();
    assert!(matches!(
        f.store.execute(
            "respond",
            &Command::MailboxRespond {
                id: delivery.into(),
                revision: 1,
                reason: "无需行动".into()
            }
        ),
        Err(Error::Forbidden(_))
    ));
}

#[test]
fn stale_intake_is_rejected_and_accepted_intake_leaves_durable_responsibility() {
    let mut f = Fixture::new(false);
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    f.complete_contract(id);
    let intake = |revision| Command::Intake {
        id: id.into(),
        revision,
        decision: IntakeDecision::Accept,
        reason: "满足承接条件".into(),
    };
    assert!(matches!(
        f.store.execute("stale", &intake(1)),
        Err(Error::Conflict(_))
    ));
    let accepted = f.store.execute("accepted", &intake(2)).unwrap();
    assert_eq!(accepted["task"]["state"], "active");
    let queue = f.store.mailbox(None).unwrap();
    assert_eq!(queue[0]["status"], "cancelled");
    assert_eq!(queue[1]["status"], "handled");
    assert_eq!(queue[2]["status"], "queued");
    assert_eq!(queue[2]["message"]["kind"], "result");
    assert_eq!(f.store.execute("accepted", &intake(2)).unwrap(), accepted);
    assert!(
        f.store
            .execute(
                "frozen",
                &Command::TaskUpdate {
                    decision_id: None,
                    id: id.into(),
                    revision: 3,
                    goal: Some("更换目标".into()),
                    contract: Default::default(),
                    refresh_team: true
                }
            )
            .is_err()
    );
}

#[test]
fn pending_refresh_is_explicit_and_preserves_used_budget() {
    let mut f = Fixture::new(false);
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    let patch = TeamPatch {
        id: f.team.id.clone(),
        revision: 1,
        name: "新团队说明".into(),
        members: f.team.members.clone(),
        leader: f.team.leader.clone(),
        executor: f.team.executor.clone(),
        verifier: f.team.verifier.clone(),
        grants: BTreeMap::new(),
    };
    f.store
        .execute("change-team", &Command::TeamUpdate { patch })
        .unwrap();
    assert_eq!(f.store.task(id).unwrap().team_snapshot.revision, 1);
    let refreshed = f
        .store
        .execute(
            "refresh",
            &Command::TaskUpdate {
                decision_id: None,
                id: id.into(),
                revision: 1,
                goal: None,
                contract: Default::default(),
                refresh_team: true,
            },
        )
        .unwrap();
    assert_eq!(refreshed["task"]["team_snapshot"]["revision"], 2);
    assert_eq!(refreshed["task"]["messages_used"], 2);
    assert_eq!(f.store.mailbox(None).unwrap()[0]["status"], "cancelled");
}

#[test]
fn replay_does_not_depend_on_current_patch_defaults() {
    let mut f = Fixture::new(false);
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    let patch = Command::TaskUpdate {
        decision_id: None,
        id: id.into(),
        revision: 1,
        goal: Some("目标一".into()),
        contract: Default::default(),
        refresh_team: false,
    };
    let first = f.store.execute("patch", &patch).unwrap();
    f.store
        .execute(
            "patch2",
            &Command::TaskUpdate {
                decision_id: None,
                id: id.into(),
                revision: 2,
                goal: Some("目标二".into()),
                contract: Default::default(),
                refresh_team: false,
            },
        )
        .unwrap();
    assert_eq!(f.store.execute("patch", &patch).unwrap(), first);
    assert_eq!(f.store.task(id).unwrap().goal, "目标二");
}

#[test]
fn identical_new_messages_consume_budget_but_transport_replay_does_not() {
    let mut f = Fixture::new(false);
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    f.store
        .execute(
            "limit",
            &Command::TaskUpdate {
                decision_id: None,
                id: id.into(),
                revision: 1,
                goal: None,
                contract: ContractPatch {
                    max_messages: Some(3),
                    ..Default::default()
                },
                refresh_team: false,
            },
        )
        .unwrap();
    let command = Command::MessageSend {
        task_id: id.into(),
        recipient: f.human,
        kind: "work.note".into(),
        body: "问题".into(),
        reply_to: None,
    };
    let first = f.store.execute("m1", &command).unwrap();
    assert_eq!(f.store.execute("m1", &command).unwrap(), first);
    assert!(matches!(
        f.store.execute("m2", &command),
        Err(Error::Conflict(_))
    ));
    assert_eq!(f.store.task(id).unwrap().messages_used, 3);
    drop(f.store);
    let mut store = Store::open(f.dir.path()).unwrap();
    assert!(store.execute("m3", &command).is_err());
}

#[test]
fn same_request_with_different_content_conflicts() {
    let mut f = Fixture::new(false);
    f.create();
    assert!(matches!(
        f.store.execute(
            "task",
            &Command::TaskCreate {
                team_id: f.team.id,
                goal: "different".into()
            }
        ),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn revoked_authorization_blocks_replay_and_cached_result_lookup() {
    let mut f = Fixture::new(false);
    let original = f.create();
    f.store
        .execute(
            "revoke",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 1,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(f.human.clone(), vec![Permission::Communicate])]),
            },
        )
        .unwrap();
    assert!(matches!(f.store.request("task"), Err(Error::Forbidden(_))));
    assert!(matches!(
        f.store.execute(
            "task",
            &Command::TaskCreate {
                team_id: f.team.id,
                goal: "离线双人井字棋".into()
            }
        ),
        Err(Error::Forbidden(_))
    ));
    assert_eq!(
        f.store.list("task").unwrap()[0]["id"],
        original["task"]["id"]
    );
}

#[test]
fn configuration_change_does_not_change_accepted_task_snapshot() {
    let mut f = Fixture::new(false);
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    f.complete_contract(id);
    f.store
        .execute(
            "intake",
            &Command::Intake {
                id: id.into(),
                revision: 2,
                decision: IntakeDecision::Accept,
                reason: "可以承接".into(),
            },
        )
        .unwrap();
    let patch = TeamPatch {
        id: f.team.id.clone(),
        revision: 1,
        name: "更新后的团队".into(),
        members: f.team.members.clone(),
        leader: f.human.clone(),
        executor: f.team.verifier.clone(),
        verifier: f.team.executor.clone(),
        grants: BTreeMap::new(),
    };
    f.store
        .execute("team-update", &Command::TeamUpdate { patch })
        .unwrap();
    let frozen = f.store.task(id).unwrap();
    assert_eq!(frozen.team_snapshot.revision, 1);
    assert_eq!(frozen.team_snapshot.executor, f.team.executor);
    let next = f
        .store
        .execute(
            "new-task",
            &Command::TaskCreate {
                team_id: f.team.id.clone(),
                goal: "新任务".into(),
            },
        )
        .unwrap();
    assert_eq!(next["task"]["team_snapshot"]["revision"], 2);
    let patch = TeamPatch {
        id: f.team.id,
        revision: 2,
        name: "换人".into(),
        members: f.team.members,
        leader: f.team.executor.unwrap(),
        executor: None,
        verifier: None,
        grants: BTreeMap::new(),
    };
    assert!(matches!(
        f.store
            .execute("replace-leader", &Command::TeamUpdate { patch }),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn ordinary_mailbox_response_cannot_accept_intake() {
    let mut f = Fixture::new(false);
    let task = f.create();
    assert!(
        f.store
            .execute(
                "fake",
                &Command::MailboxRespond {
                    id: task["delivery"]["deliveryId"].as_str().unwrap().into(),
                    revision: 1,
                    reason: "已承接".into()
                }
            )
            .is_err()
    );
    assert_eq!(
        f.store
            .task(task["task"]["id"].as_str().unwrap())
            .unwrap()
            .state,
        "pending"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_waiter_does_not_cancel_accepted_database_operation() {
    let f = Fixture::new(false);
    let database = Database::open(f.dir.path().into(), 1).await.unwrap();
    let client = database.client();
    let running_client = client.clone();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        running_client
            .call(move |store| {
                let _ = entered.send(());
                wait.recv().unwrap();
                store.execute(
                    "cancelled-waiter",
                    &Command::WorkerCreate {
                        connection: None,
                        name: "持久成员".into(),
                        description: String::new(),
                    },
                )
            })
            .await
    });
    ready.await.unwrap();
    task.abort();
    // The single-thread event loop can still service timers while the DB is blocked.
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::time::sleep(Duration::from_millis(10)),
    )
    .await
    .unwrap();
    release.send(()).unwrap();
    let result = client
        .call(|store| store.request("cancelled-waiter"))
        .await
        .unwrap();
    assert_eq!(result["name"], "持久成员");
    database.close().await.unwrap();
    assert!(client.call(|store| store.workspace()).await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn full_database_channel_backpressures_without_blocking_runtime() {
    let f = Fixture::new(false);
    let database = Database::open(f.dir.path().into(), 1).await.unwrap();
    let client = database.client();
    let first = client.clone();
    let second = client.clone();
    let third = client.clone();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let a = tokio::spawn(async move {
        first
            .call(move |_| {
                let _ = entered.send(());
                wait.recv().unwrap();
                Ok(1)
            })
            .await
    });
    ready.await.unwrap();
    let b = tokio::spawn(async move { second.call(|_| Ok(2)).await });
    tokio::task::yield_now().await;
    let c = tokio::spawn(async move { third.call(|_| Ok(3)).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!b.is_finished());
    assert!(!c.is_finished());
    release.send(()).unwrap();
    assert_eq!(a.await.unwrap().unwrap(), 1);
    assert_eq!(b.await.unwrap().unwrap(), 2);
    assert_eq!(c.await.unwrap().unwrap(), 3);
    database.close().await.unwrap();
}

#[test]
fn code_intake_requires_fixed_profile_and_intact_input_then_freezes_both() {
    use atelier::{content::GitInput, profile::VerificationProfile};
    let mut f = Fixture::new(false);
    let sample = tempfile::tempdir().unwrap();
    let prepared = atelier::sample::prepare(sample.path()).unwrap();
    let imported = f
        .store
        .execute(
            "input",
            &Command::InputImport {
                repository: sample.path().to_str().unwrap().into(),
                commit: prepared["commit"].as_str().unwrap().into(),
            },
        )
        .unwrap();
    let input: GitInput = serde_json::from_value(imported).unwrap();
    let id = f.create()["task"]["id"].as_str().unwrap().to_owned();
    f.complete_contract(&id);
    f.store
        .execute(
            "bind-input",
            &Command::TaskUpdate {
                decision_id: None,
                id: id.clone(),
                revision: 2,
                goal: None,
                contract: ContractPatch {
                    code_input: Some(input.id.clone()),
                    ..Default::default()
                },
                refresh_team: false,
            },
        )
        .unwrap();
    let accept = Command::Intake {
        id: id.clone(),
        revision: 3,
        decision: IntakeDecision::Accept,
        reason: "检查前置".into(),
    };
    assert!(matches!(
        f.store.execute("accept", &accept),
        Err(Error::Invalid(_))
    ));
    assert_eq!(f.store.task(&id).unwrap().state, "pending");
    let profile = f
        .store
        .execute(
            "profile",
            &Command::ProfileImport {
                specification: VerificationProfile {
                    name: "公开测试配置（未准备运行环境）".into(),
                    check_id: "tic-tac-toe".into(),
                    image: format!("sha256:{}", "a".repeat(64)),
                    argv: vec![
                        "node".into(),
                        "/checks/check.mjs".into(),
                        "/candidate".into(),
                    ],
                },
            },
        )
        .unwrap();
    let profile_id = profile["id"].as_str().unwrap();
    f.store
        .execute(
            "bind-profile",
            &Command::TaskUpdate {
                decision_id: None,
                id: id.clone(),
                revision: 3,
                goal: None,
                contract: ContractPatch {
                    verification_profile: Some(profile_id.into()),
                    ..Default::default()
                },
                refresh_team: false,
            },
        )
        .unwrap();
    let file = &input.files[0];
    let blob = f.dir.path().join("objects/blobs").join(&file.sha256);
    let original = std::fs::read(&blob).unwrap();
    std::fs::write(&blob, b"corrupted").unwrap();
    let accept = Command::Intake {
        id: id.clone(),
        revision: 4,
        decision: IntakeDecision::Accept,
        reason: "固定输入已确认".into(),
    };
    assert!(matches!(
        f.store.execute("accept", &accept),
        Err(Error::Conflict(_))
    ));
    assert!(f.store.request("accept").is_err());
    assert_eq!(f.store.task(&id).unwrap().revision, 4);
    std::fs::write(&blob, original).unwrap();
    drop(sample); // Intake must not read the now-deleted source repository.
    let accepted = f.store.execute("accept", &accept).unwrap();
    assert_eq!(accepted["task"]["state"], "active");
    let change = Command::TaskUpdate {
        decision_id: None,
        id: id.clone(),
        revision: 5,
        goal: None,
        contract: ContractPatch {
            verification_profile: Some(profile_id.into()),
            ..Default::default()
        },
        refresh_team: false,
    };
    assert!(matches!(
        f.store.execute("late", &change),
        Err(Error::Conflict(_))
    ));
    let task = f.store.task(&id).unwrap();
    assert_eq!(task.contract.code_input.as_deref(), Some(input.id.as_str()));
    assert_eq!(
        task.contract.verification_profile.as_deref(),
        Some(profile_id)
    );
}

#[test]
fn failed_input_publication_leaves_no_reference_and_retry_can_commit() {
    let f = Fixture::new(false);
    let sample = tempfile::tempdir().unwrap();
    let prepared = atelier::sample::prepare(sample.path()).unwrap();
    let command = Command::InputImport {
        repository: sample.path().to_str().unwrap().into(),
        commit: prepared["commit"].as_str().unwrap().into(),
    };
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_import BEFORE INSERT ON requests BEGIN SELECT RAISE(ABORT,'injected write failure'); END;").unwrap();
    let mut store = f.store;
    assert!(store.execute("import", &command).is_err());
    assert_eq!(store.list("input").unwrap(), json!([]));
    assert!(store.request("import").is_err());
    assert!(f.dir.path().join("objects/blobs").is_dir());
    db.execute_batch("DROP TRIGGER reject_import;").unwrap();
    let saved = store.execute("import", &command).unwrap();
    drop(sample);
    drop(store);
    let mut store = Store::open(f.dir.path()).unwrap();
    assert_eq!(store.execute("import", &command).unwrap(), saved);
    assert_eq!(store.list("input").unwrap().as_array().unwrap().len(), 1);
}

impl Fixture {
    fn decision_request(&mut self, task_id: &str, handler: &str, request_id: &str) -> Value {
        self.store
            .execute(
                request_id,
                &Command::DecisionRequest {
                    task_id: task_id.into(),
                    revision: self.store.task(task_id).unwrap().revision,
                    handler: handler.into(),
                    question: "是否补充离线交付要求？".into(),
                    impact: "正式回应后仍需更新任务，才会改变契约".into(),
                    options: vec!["补充".into(), "等待".into()],
                },
            )
            .unwrap()
    }
}

#[test]
fn decisions_separate_answer_blocked_implementation_and_verified_business_effect() {
    let mut f = Fixture::new(false);
    let task_id = f.create()["task"]["id"].as_str().unwrap().to_owned();
    let human = f.human.clone();
    let request = f.decision_request(&task_id, &human, "ask");
    let id = request["decision"]["id"].as_str().unwrap();
    let original = f.store.task(&task_id).unwrap();
    let answer = Command::DecisionRespond {
        id: id.into(),
        revision: 1,
        answer: "补充".into(),
    };
    let response = f.store.execute("answer", &answer).unwrap();
    assert_eq!(response["decision"]["state"], "responded");
    assert_eq!(
        f.store.task(&task_id).unwrap().contract.inputs,
        original.contract.inputs
    );
    assert_eq!(f.store.team(&f.team.id).unwrap().authorization_revision, 1);
    assert_eq!(f.store.execute("answer", &answer).unwrap(), response);
    let blocked = f
        .store
        .execute(
            "blocked",
            &Command::DecisionRecord {
                id: id.into(),
                revision: 2,
                resolution: DecisionResolution::Blocked {
                    reason: "尚未取得补充材料".into(),
                },
            },
        )
        .unwrap();
    assert_eq!(blocked["state"], "responded");
    assert_eq!(blocked["blocked_reason"], "尚未取得补充材料");
    let forged = Command::DecisionRecord {
        id: id.into(),
        revision: 3,
        resolution: DecisionResolution::Operation {
            reference: OperationReference {
                actor: human.clone(),
                request_id: "task".into(),
            },
        },
    };
    assert!(matches!(
        f.store.execute("forged", &forged),
        Err(Error::Invalid(_))
    ));
    let operation = Command::TaskUpdate {
        decision_id: Some(id.into()),
        id: task_id.clone(),
        revision: 1,
        goal: None,
        contract: ContractPatch {
            inputs: Some("只使用离线公开样例".into()),
            ..Default::default()
        },
        refresh_team: false,
    };
    f.store.execute("implement", &operation).unwrap();
    let decision = f.store.decision(id).unwrap();
    assert_eq!(decision.state, "responded");
    assert_eq!(decision.task_revision, 1);
    assert_eq!(decision.effective_revision, 2);
    assert!(decision.blocked_reason.is_none());
    assert_eq!(decision.revision, 4);
    assert!(
        f.store
            .execute(
                "pretend-no-change",
                &Command::DecisionRecord {
                    id: id.into(),
                    revision: 4,
                    resolution: DecisionResolution::NoChange {
                        reason: "不能忽略已有变更".into()
                    }
                }
            )
            .is_err()
    );
    let resolved = f
        .store
        .execute(
            "record",
            &Command::DecisionRecord {
                id: id.into(),
                revision: 4,
                resolution: DecisionResolution::Operation {
                    reference: OperationReference {
                        actor: human,
                        request_id: "implement".into(),
                    },
                },
            },
        )
        .unwrap();
    assert_eq!(resolved["state"], "resolved");
    let mailbox = f.store.mailbox(None).unwrap();
    for delivery in [
        request["decision"]["request_delivery"].as_str().unwrap(),
        response["decision"]["response_delivery"].as_str().unwrap(),
    ] {
        assert_eq!(
            mailbox
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == delivery)
                .unwrap()["status"],
            "handled"
        );
    }
    assert_eq!(f.store.task(&task_id).unwrap().state, "pending");
}

#[test]
fn stale_decisions_are_superseded_and_cannot_be_closed_by_chat_or_other_handler() {
    let mut f = Fixture::new(true);
    let task_id = f.create()["task"]["id"].as_str().unwrap().to_owned();
    let leader = f.team.leader.clone();
    let human = f.human.clone();
    let other = f.decision_request(&task_id, &leader, "other-handler");
    let denied = f.store.execute(
        "wrong-handler",
        &Command::DecisionRespond {
            id: other["decision"]["id"].as_str().unwrap().into(),
            revision: 1,
            answer: "补充".into(),
        },
    );
    assert!(matches!(denied, Err(Error::Forbidden(_))));
    let request = f.decision_request(&task_id, &human, "self-handler");
    let id = request["decision"]["id"].as_str().unwrap();
    f.store
        .execute(
            "ordinary-message",
            &Command::MessageSend {
                task_id: task_id.clone(),
                recipient: human.clone(),
                kind: "work.note".into(),
                body: "同意补充，已经落实".into(),
                reply_to: None,
            },
        )
        .unwrap();
    assert_eq!(f.store.decision(id).unwrap().state, "open");
    assert!(
        f.store
            .execute(
                "mailbox-close",
                &Command::MailboxRespond {
                    id: request["decision"]["request_delivery"]
                        .as_str()
                        .unwrap()
                        .into(),
                    revision: 1,
                    reason: "同意".into()
                }
            )
            .is_err()
    );
    f.store
        .execute(
            "update",
            &Command::TaskUpdate {
                decision_id: None,
                id: task_id.clone(),
                revision: 1,
                goal: Some("已变化的依据".into()),
                contract: ContractPatch::default(),
                refresh_team: false,
            },
        )
        .unwrap();
    assert_eq!(f.store.decision(id).unwrap().state, "superseded");
    assert!(
        f.store
            .execute(
                "stale-response",
                &Command::DecisionRespond {
                    id: id.into(),
                    revision: 2,
                    answer: "补充".into()
                }
            )
            .is_err()
    );
    assert!(f.store.request("stale-response").is_err());
    let new_request = f.decision_request(&task_id, &human, "new-question");
    f.store
        .execute(
            "cancel",
            &Command::TaskCancel {
                id: task_id.clone(),
                revision: 2,
                reason: "结束测试".into(),
            },
        )
        .unwrap();
    assert_eq!(
        f.store
            .decision(new_request["decision"]["id"].as_str().unwrap())
            .unwrap()
            .state,
        "superseded"
    );
}

#[test]
fn decision_response_and_result_delivery_roll_back_together() {
    let mut f = Fixture::new(false);
    let task_id = f.create()["task"]["id"].as_str().unwrap().to_owned();
    let human = f.human.clone();
    let request = f.decision_request(&task_id, &human, "ask");
    let id = request["decision"]["id"].as_str().unwrap();
    let used = f.store.task(&task_id).unwrap().messages_used;
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_decision_result BEFORE INSERT ON messages WHEN NEW.kind='decision.result' BEGIN SELECT RAISE(ABORT,'injected result failure'); END;").unwrap();
    let respond = Command::DecisionRespond {
        id: id.into(),
        revision: 1,
        answer: "补充".into(),
    };
    assert!(f.store.execute("respond", &respond).is_err());
    assert_eq!(f.store.decision(id).unwrap().state, "open");
    assert_eq!(f.store.task(&task_id).unwrap().messages_used, used);
    assert!(f.store.request("respond").is_err());
    let deliveries = f.store.mailbox(None).unwrap();
    assert_eq!(
        deliveries
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == request["decision"]["request_delivery"])
            .unwrap()["status"],
        "queued"
    );
    db.execute_batch("DROP TRIGGER fail_decision_result;")
        .unwrap();
    f.store.execute("respond", &respond).unwrap();
    assert_eq!(f.store.task(&task_id).unwrap().messages_used, used + 1);
    let no_change = f
        .store
        .execute(
            "record",
            &Command::DecisionRecord {
                id: id.into(),
                revision: 2,
                resolution: DecisionResolution::NoChange {
                    reason: "答复仅补充说明，无需修改业务对象".into(),
                },
            },
        )
        .unwrap();
    assert_eq!(no_change["state"], "resolved");
    assert_eq!(no_change["operation"], Value::Null);
}

#[test]
fn decision_does_not_grant_permissions_and_records_only_explicit_authorized_change() {
    let mut f = Fixture::new(false);
    let task_id = f.create()["task"]["id"].as_str().unwrap().to_owned();
    let human = f.human.clone();
    let request = f.decision_request(&task_id, &human, "ask");
    let id = request["decision"]["id"].as_str().unwrap();
    f.store
        .execute(
            "respond",
            &Command::DecisionRespond {
                id: id.into(),
                revision: 1,
                answer: "补充".into(),
            },
        )
        .unwrap();
    assert!(
        !f.store
            .team(&f.team.id)
            .unwrap()
            .grants
            .contains_key(f.team.executor.as_ref().unwrap())
    );
    let grant = Command::PermissionsUpdate {
        decision_id: Some(id.into()),
        team_id: f.team.id.clone(),
        revision: 1,
        grant: BTreeMap::from([(f.team.executor.clone().unwrap(), vec![Permission::Execute])]),
        revoke: BTreeMap::new(),
    };
    f.store.execute("grant", &grant).unwrap();
    assert_eq!(f.store.decision(id).unwrap().state, "responded");
    let record = Command::DecisionRecord {
        id: id.into(),
        revision: 3,
        resolution: DecisionResolution::Operation {
            reference: OperationReference {
                actor: human.clone(),
                request_id: "grant".into(),
            },
        },
    };
    assert_eq!(
        f.store.execute("record", &record).unwrap()["state"],
        "resolved"
    );
    f.store
        .execute(
            "revoke-communicate",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 2,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(human, vec![Permission::Communicate])]),
            },
        )
        .unwrap();
    assert!(matches!(
        f.store.execute("record", &record),
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        f.store.request("record"),
        Err(Error::Forbidden(_))
    ));
}

#[test]
fn execution_configuration_is_immutable_and_task_refresh_is_explicit() {
    use atelier::connection::{ApiProtocol, ConnectionSpec};
    let mut f = Fixture::new(false);
    let create = Command::ConnectionCreate {
        name: "模型连接".into(),
        specification: ConnectionSpec::Api {
            protocol: ApiProtocol::OpenaiChatCompletions,
            model: "model-one".into(),
            base_url: Some("https://example.invalid/v1".into()),
        },
    };
    let connection = f.store.execute("connection", &create).unwrap();
    let connection_id = connection["connection"]["id"].as_str().unwrap();
    let old_version = connection["version"]["id"].as_str().unwrap();
    let worker_id = f.team.executor.clone().unwrap();
    let bind = Command::WorkerUpdate {
        id: worker_id.clone(),
        revision: 1,
        name: None,
        description: None,
        connection: Some(connection_id.into()),
        clear_connection: false,
    };
    let worker = f.store.execute("bind", &bind).unwrap();
    let old_config = worker["execution_config"].as_str().unwrap().to_owned();
    let task_id = f.create()["task"]["id"].as_str().unwrap().to_owned();
    f.store
        .execute(
            "change-connection",
            &Command::ConnectionUpdate {
                id: connection_id.into(),
                revision: 1,
                name: None,
                specification: Some(ConnectionSpec::Api {
                    protocol: ApiProtocol::OpenaiChatCompletions,
                    model: "model-two".into(),
                    base_url: Some("https://example.invalid/v1".into()),
                }),
            },
        )
        .unwrap();
    assert_eq!(
        f.store
            .worker(&worker_id)
            .unwrap()
            .execution_config
            .as_deref(),
        Some(old_config.as_str())
    );
    let worker = f
        .store
        .execute(
            "rebind",
            &Command::WorkerUpdate {
                id: worker_id.clone(),
                revision: 2,
                name: None,
                description: None,
                connection: Some(connection_id.into()),
                clear_connection: false,
            },
        )
        .unwrap();
    let new_config = worker["execution_config"].as_str().unwrap();
    assert_ne!(old_config, new_config);
    assert_eq!(
        f.store.task(&task_id).unwrap().worker_snapshots[&worker_id]
            .execution_config
            .as_deref(),
        Some(old_config.as_str())
    );
    f.store
        .execute(
            "refresh",
            &Command::TaskUpdate {
                decision_id: None,
                id: task_id.clone(),
                revision: 1,
                goal: None,
                contract: ContractPatch {
                    inputs: Some("公开资料".into()),
                    delivery: Some("报告".into()),
                    verification: Some("独立检验".into()),
                    ..Default::default()
                },
                refresh_team: true,
            },
        )
        .unwrap();
    assert_eq!(
        f.store.task(&task_id).unwrap().worker_snapshots[&worker_id]
            .execution_config
            .as_deref(),
        Some(new_config)
    );
    f.store
        .execute(
            "accept",
            &Command::Intake {
                id: task_id.clone(),
                revision: 2,
                decision: IntakeDecision::Accept,
                reason: "契约明确".into(),
            },
        )
        .unwrap();
    f.store
        .execute(
            "clear-current",
            &Command::WorkerUpdate {
                id: worker_id.clone(),
                revision: 3,
                name: None,
                description: None,
                connection: None,
                clear_connection: true,
            },
        )
        .unwrap();
    assert!(
        f.store
            .worker(&worker_id)
            .unwrap()
            .execution_config
            .is_none()
    );
    assert_eq!(
        f.store.task(&task_id).unwrap().worker_snapshots[&worker_id]
            .execution_config
            .as_deref(),
        Some(new_config)
    );
    assert_eq!(
        f.store
            .execution_configuration(&old_config)
            .unwrap()
            .connection_version,
        old_version
    );
    assert_eq!(
        f.store.execute("bind", &bind).unwrap(),
        json!(f.store.request("bind").unwrap())
    );
}

#[test]
fn connection_fields_are_exclusive_and_secrets_are_not_configuration() {
    use atelier::connection::{ApiProtocol, ConnectionSpec};
    let mut f = Fixture::new(false);
    for input in [
        json!({"transport":"api","protocol":"openai-chat-completions","model":"m","runtime":"pi"}),
        json!({"transport":"agent-cli","runtime":"pi","protocol":"openai-chat-completions"}),
        json!({"transport":"api","protocol":"openai-chat-completions","model":"m","api_key":"not-a-real-secret"}),
        json!({"transport":"api","protocol":"openai-chat-completions"}),
    ] {
        assert!(serde_json::from_value::<ConnectionSpec>(input).is_err());
    }
    for url in [
        "http://example.invalid/v1",
        "https://name:credential@example.invalid",
        "https://example.invalid/?api_key=synthetic",
        "https://example.invalid/#key",
    ] {
        assert!(
            f.store
                .execute(
                    "invalid",
                    &Command::ConnectionCreate {
                        name: "invalid".into(),
                        specification: ConnectionSpec::Api {
                            protocol: ApiProtocol::OpenaiChatCompletions,
                            model: "m".into(),
                            base_url: Some(url.into())
                        }
                    }
                )
                .is_err()
        );
    }
    assert_eq!(f.store.list("connection").unwrap(), json!([]));
    let cli = f
        .store
        .execute(
            "cli",
            &Command::ConnectionCreate {
                name: "独立 CLI 登录".into(),
                specification: ConnectionSpec::AgentCli {
                    runtime: "pi".into(),
                    model: None,
                    image: None,
                    egress_hosts: None,
                    egress_proxy: None,
                },
            },
        )
        .unwrap();
    assert_eq!(cli["readiness"], "unchecked");
    assert_eq!(f.store.list("task").unwrap(), json!([]));
    assert!(
        f.store
            .execute(
                "human-config",
                &Command::WorkerUpdate {
                    id: f.human.clone(),
                    revision: 1,
                    name: None,
                    description: None,
                    connection: Some(cli["connection"]["id"].as_str().unwrap().into()),
                    clear_connection: false
                }
            )
            .is_err()
    );
}

impl Fixture {
    fn prepare_digital_leader(&mut self) -> String {
        use atelier::connection::{ApiProtocol, ConnectionSpec};
        let connection = self
            .store
            .execute(
                "leader-connection",
                &Command::ConnectionCreate {
                    name: "API fixture 配置（不调用模型）".into(),
                    specification: ConnectionSpec::Api {
                        protocol: ApiProtocol::OpenaiChatCompletions,
                        model: "fixture-model".into(),
                        base_url: Some("https://example.invalid/v1".into()),
                    },
                },
            )
            .unwrap();
        let worker = self
            .store
            .execute(
                "leader-config",
                &Command::WorkerUpdate {
                    id: self.team.leader.clone(),
                    revision: 1,
                    name: None,
                    description: None,
                    connection: Some(connection["connection"]["id"].as_str().unwrap().into()),
                    clear_connection: false,
                },
            )
            .unwrap();
        self.store
            .execute(
                "leader-grants",
                &Command::PermissionsUpdate {
                    decision_id: None,
                    team_id: self.team.id.clone(),
                    revision: self.store.team(&self.team.id).unwrap().revision,
                    grant: BTreeMap::from([(
                        self.team.leader.clone(),
                        vec![Permission::Arrange, Permission::Communicate],
                    )]),
                    revoke: BTreeMap::new(),
                },
            )
            .unwrap();
        worker["execution_config"].as_str().unwrap().into()
    }
}

#[test]
fn concurrent_claims_atomically_create_one_run_and_consume_one_budget_unit() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let delivery = created["delivery"]["deliveryId"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = created["task"]["id"].as_str().unwrap();
    f.store.runtime_register("service-one", 123).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = f.dir.path().to_path_buf();
        let barrier = barrier.clone();
        let delivery = delivery.clone();
        let config = config.clone();
        handles.push(std::thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            barrier.wait();
            store.runtime_claim("service-one", &delivery, &config)
        }));
    }
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let run = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(run.state, "prepared");
    assert!(!run.launch_started);
    assert_eq!(f.store.task(task).unwrap().runs_used, 1);
    let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
    let receipt = mailbox
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == delivery)
        .unwrap();
    assert_eq!(receipt["status"], "claimed");
    assert_eq!(receipt["runId"], run.id);
    assert_eq!(receipt["attempts"], 1);
}

#[test]
fn receipt_write_failure_rolls_back_run_acceptance_and_budget() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let delivery = created["delivery"]["deliveryId"].as_str().unwrap();
    let task = created["task"]["id"].as_str().unwrap();
    f.store.runtime_register("service-one", 123).unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER claim_failure BEFORE UPDATE ON deliveries WHEN NEW.status='claimed' BEGIN SELECT RAISE(ABORT,'injected claim failure'); END;").unwrap();
    assert!(
        f.store
            .runtime_claim("service-one", delivery, &config)
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(f.store.task(task).unwrap().runs_used, 0);
    assert_eq!(
        f.store.mailbox(Some(&f.team.leader)).unwrap()[0]["status"],
        "queued"
    );
    db.execute_batch("DROP TRIGGER claim_failure;").unwrap();
    assert!(
        f.store
            .runtime_claim("service-one", delivery, &config)
            .is_ok()
    );
}

#[test]
fn restart_after_launch_intent_preserves_unknown_and_rejects_new_runs_and_old_epoch() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let delivery = created["delivery"]["deliveryId"].as_str().unwrap();
    let task = created["task"]["id"].as_str().unwrap();
    f.store.runtime_register("service-one", 123).unwrap();
    let run = f
        .store
        .runtime_claim("service-one", delivery, &config)
        .unwrap();
    f.store
        .runtime_begin_launch("service-one", &run.id)
        .unwrap();
    // Fault injection at the launch-intent/child-registration boundary. This
    // proves durable state gates, not actual process/container reconciliation.
    drop(f.store);
    f.store = Store::open(f.dir.path()).unwrap();
    f.store.runtime_register("service-two", 456).unwrap();
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    assert_eq!(
        f.store.mailbox(Some(&f.team.leader)).unwrap()[0]["status"],
        "uncertain"
    );
    assert!(
        f.store
            .runtime_child_started("service-one", &run.id, 100, "old-start")
            .is_err()
    );
    assert!(
        f.store
            .runtime_run_observed_stopped("service-one", &run.id, "旧服务不能决定新状态")
            .is_err()
    );
    let another = f
        .store
        .execute(
            "another-task",
            &Command::TaskCreate {
                team_id: f.team.id.clone(),
                goal: "另一个任务".into(),
            },
        )
        .unwrap();
    assert!(
        f.store
            .runtime_claim(
                "service-two",
                another["delivery"]["deliveryId"].as_str().unwrap(),
                &config
            )
            .is_err()
    );
    assert!(
        f.store
            .execute(
                "unsafe-refresh",
                &Command::TaskUpdate {
                    decision_id: None,
                    id: task.into(),
                    revision: 1,
                    goal: None,
                    contract: ContractPatch::default(),
                    refresh_team: true
                }
            )
            .is_err()
    );
    let cancelling = f
        .store
        .execute(
            "cancel",
            &Command::TaskCancel {
                id: task.into(),
                revision: 1,
                reason: "请求取消".into(),
            },
        )
        .unwrap();
    assert_eq!(cancelling["state"], "pending");
    assert_eq!(cancelling["cancellation_requested"], true);
    assert!(f.store.runtime_stopped("service-two").is_err());
    // Inject an authoritative stopped observation; its real resource provider
    // is still an integration requirement and is not simulated as a live run.
    f.store
        .runtime_run_observed_stopped("service-two", &run.id, "测试注入：资源已核对停止")
        .unwrap();
    assert_eq!(f.store.task(task).unwrap().state, "closed");
    assert_eq!(
        f.store.task(task).unwrap().outcome.as_deref(),
        Some("cancelled")
    );
    assert_eq!(f.store.task(task).unwrap().runs_used, 1);
    assert!(
        f.store
            .runtime_claim(
                "service-two",
                another["delivery"]["deliveryId"].as_str().unwrap(),
                &config
            )
            .is_ok()
    );
}

#[test]
fn never_launched_prepared_run_is_stopped_on_restart_without_requeuing_or_refunding() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    f.store.runtime_register("service-one", 123).unwrap();
    let run = f
        .store
        .runtime_claim(
            "service-one",
            created["delivery"]["deliveryId"].as_str().unwrap(),
            &config,
        )
        .unwrap();
    f.store.runtime_register("service-two", 456).unwrap();
    assert_eq!(f.store.run(&run.id).unwrap().state, "stopped");
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
    assert_eq!(
        f.store.mailbox(Some(&f.team.leader)).unwrap()[0]["status"],
        "blocked"
    );
    assert!(
        f.store
            .runtime_claim("service-two", &run.delivery_id, &config)
            .is_err()
    );
}

#[test]
fn revocation_and_runtime_stop_block_launch_and_restoring_grants_does_not_revive_run() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    f.store.runtime_register("service-one", 123).unwrap();
    let run = f
        .store
        .runtime_claim(
            "service-one",
            created["delivery"]["deliveryId"].as_str().unwrap(),
            &config,
        )
        .unwrap();
    let revoke = f
        .store
        .execute(
            "revoke",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 2,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(f.team.leader.clone(), vec![Permission::Arrange])]),
            },
        )
        .unwrap();
    assert_eq!(revoke["activeRuns"], 1);
    assert!(f.store.run(&run.id).unwrap().stop_requested);
    assert!(
        f.store
            .runtime_begin_launch("service-one", &run.id)
            .is_err()
    );
    f.store
        .execute(
            "restore",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 3,
                grant: BTreeMap::from([(f.team.leader.clone(), vec![Permission::Arrange])]),
                revoke: BTreeMap::new(),
            },
        )
        .unwrap();
    assert!(
        f.store
            .runtime_begin_launch("service-one", &run.id)
            .is_err()
    );
    f.store.runtime_request_stop("service-one").unwrap();
    assert!(f.store.runtime_stopped("service-one").is_err());
    f.store
        .runtime_run_observed_stopped("service-one", &run.id, "测试注入：没有启动资源")
        .unwrap();
    f.store.runtime_stopped("service-one").unwrap();
    assert_eq!(f.store.runtime_snapshot().unwrap()["state"], "stopped");
    assert_eq!(f.store.task(&run.task_id).unwrap().state, "pending");
}

#[test]
fn run_failure_notifications_are_atomic_durable_and_deduplicated() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let intake = &f.store.mailbox(Some(&f.team.leader)).unwrap()[0]["message"];
    assert_eq!(intake["source"], "core");
    assert_eq!(intake["sender"], f.human);
    f.store.runtime_register("service", 123).unwrap();
    let run = f
        .store
        .runtime_claim(
            "service",
            created["delivery"]["deliveryId"].as_str().unwrap(),
            &config,
        )
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_notification BEFORE INSERT ON messages WHEN NEW.kind='decision.request' BEGIN SELECT RAISE(ABORT,'injected notification failure'); END;").unwrap();
    assert!(
        f.store
            .runtime_run_unknown("service", &run.id, "测试注入：进程失联")
            .is_err()
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "prepared");
    assert_eq!(
        f.store.mailbox(Some(&f.team.leader)).unwrap()[0]["status"],
        "claimed"
    );
    assert!(
        f.store
            .mailbox(None)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    db.execute_batch("DROP TRIGGER fail_notification;").unwrap();
    f.store
        .runtime_run_unknown("service", &run.id, "测试注入：进程失联")
        .unwrap();
    f.store
        .runtime_run_unknown("service", &run.id, "测试注入：仍未确认")
        .unwrap();
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试注入：资源已停止")
        .unwrap();
    f.store
        .runtime_run_observed_stopped("service", &run.id, "重复观测")
        .unwrap();
    drop(f.store);
    f.store = Store::open(f.dir.path()).unwrap();
    {
        let mailbox = f.store.mailbox(Some(&f.human)).unwrap();
        let failures: Vec<_> = mailbox
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["message"]["kind"] == "decision.request")
            .collect();
        assert_eq!(failures.len(), 1);
        let message = &failures[0]["message"];
        assert_eq!(message["source"], "core");
        assert!(message["sender"].is_null());
        assert_eq!(message["causationId"], format!("run:{}", run.id));
        let body: Value = serde_json::from_str(message["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["runId"], run.id);
        assert_eq!(body["kind"], "recovery");
        assert_eq!(body["deliveryId"], run.delivery_id);
    }
    let leader_mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
    assert_eq!(
        leader_mailbox.as_array().unwrap().len(),
        1,
        "失败的负责人保留原投递，不通过新的失败投递再次唤起自己"
    );
    assert_eq!(leader_mailbox[0]["status"], "blocked");
    assert_eq!(
        f.store.task(&run.task_id).unwrap().messages_used,
        created["task"]["messages_used"].as_u64().unwrap() as u32
    );
}

#[test]
fn exhausted_run_budget_preserves_human_failure_without_new_leader_delivery() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    f.store
        .execute(
            "one-run",
            &Command::TaskUpdate {
                decision_id: None,
                id: id.into(),
                revision: 1,
                goal: None,
                contract: ContractPatch {
                    max_runs: Some(1),
                    max_messages: Some(2),
                    ..Default::default()
                },
                refresh_team: false,
            },
        )
        .unwrap();
    let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
    let delivery = mailbox
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["status"] == "queued")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    f.store.runtime_register("service", 123).unwrap();
    let run = f.store.runtime_claim("service", delivery, &config).unwrap();
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试注入：额度已耗尽")
        .unwrap();
    assert_eq!(
        f.store.mailbox(None).unwrap()[0]["message"]["kind"],
        "decision.request"
    );
    assert!(
        !f.store
            .mailbox(Some(&f.team.leader))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["message"]["kind"] == "failure")
    );
    assert_eq!(f.store.task(id).unwrap().messages_used, 2);
}

#[test]
fn claim_requires_frozen_role_even_when_receiver_has_execute_and_verify_permissions() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    f.store
        .execute(
            "extra-permissions",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 2,
                grant: BTreeMap::from([(
                    f.team.leader.clone(),
                    vec![Permission::Execute, Permission::Verify],
                )]),
                revoke: BTreeMap::new(),
            },
        )
        .unwrap();
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    let mut task = f.store.task(id).unwrap();
    task.state = "active".into();
    // Inject inconsistent internal records to check the final claim boundary.
    // Normal business operations do not create these role-mismatched messages.
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute(
        "UPDATE tasks SET state='active',data=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_string(&task).unwrap(), id],
    )
    .unwrap();
    f.store.runtime_register("service", 123).unwrap();
    for kind in ["assignment.execute", "assignment.rework", "handoff.verify"] {
        db.execute("UPDATE messages SET kind=?1 WHERE task_id=?2", [kind, id])
            .unwrap();
        let result = f.store.runtime_claim(
            "service",
            created["delivery"]["deliveryId"].as_str().unwrap(),
            &config,
        );
        assert!(matches!(result, Err(Error::Forbidden(message)) if message.contains("职责")));
    }
    assert_eq!(f.store.task(id).unwrap().runs_used, 0);
    assert_eq!(f.store.runtime_snapshot().unwrap()["activeRuns"], 0);
}

impl Fixture {
    fn running_member(&mut self) -> (Run, atelier::member::MemberBinding) {
        self.running_member_with_contract(None)
    }
    fn running_member_with_contract(
        &mut self,
        contract: Option<ContractPatch>,
    ) -> (Run, atelier::member::MemberBinding) {
        let config = self.prepare_digital_leader();
        let mut created = self.create();
        if let Some(contract) = contract {
            created = self
                .store
                .execute(
                    "contract",
                    &Command::TaskUpdate {
                        decision_id: None,
                        id: created["task"]["id"].as_str().unwrap().into(),
                        revision: 1,
                        goal: None,
                        contract,
                        refresh_team: false,
                    },
                )
                .unwrap();
        }
        self.store.runtime_register("service", 123).unwrap();
        let run = self
            .store
            .runtime_claim(
                "service",
                created["delivery"]["deliveryId"].as_str().unwrap(),
                &config,
            )
            .unwrap();
        assert!(self.store.bind_member("service", &run.id).is_err());
        self.store.runtime_begin_launch("service", &run.id).unwrap();
        // Inject child identity for the core boundary test; no actual model is started.
        let run = self
            .store
            .runtime_child_started("service", &run.id, 321, "fixture-child")
            .unwrap();
        let binding = self.store.bind_member("service", &run.id).unwrap();
        (run, binding)
    }
}

fn member_operation(
    run: &Run,
    id: &str,
    name: &str,
    input: Value,
) -> atelier::member::ToolOperation {
    atelier::member::ToolOperation {
        operation_id: format!("op-{id}"),
        originating_run_id: run.id.clone(),
        tool_call_id: format!("call-{id}"),
        name: name.into(),
        input,
    }
}

#[test]
fn member_binding_supplies_identity_and_rejects_management_or_cross_task_parameters() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    let read = member_operation(&run, "read", "task_read", json!({}));
    let data = f.store.member_call(&binding, &read).unwrap();
    assert_eq!(data["data"]["id"], run.task_id);
    assert!(data["data"].get("worker_snapshots").is_none());
    for (index, (name, input)) in [
        ("permissions_update", json!({})),
        ("task_read", json!({"taskId":"another-task"})),
        (
            "message_send",
            json!({"recipient":f.human,"kind":"work.note","body":"伪造","actor":f.human}),
        ),
        (
            "message_send",
            json!({"recipient":f.human,"kind":"assignment.execute","body":"普通工具不能安排执行"}),
        ),
        (
            "message_send",
            json!({"recipient":f.team.executor,"kind":"work.note","body":"pending 未参与者"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let operation = member_operation(&run, &format!("invalid-{index}"), name, input);
        assert_eq!(
            f.store.member_call(&binding, &operation).unwrap()["ok"],
            false
        );
    }
    assert!(
        f.store
            .mailbox(None)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let send = member_operation(
        &run,
        "send",
        "message_send",
        json!({"recipient":f.human,"kind":"work.question","body":"需要补充验收规则"}),
    );
    assert_eq!(f.store.member_call(&binding, &send).unwrap()["ok"], true);
    let mailbox = f.store.mailbox(None).unwrap();
    assert_eq!(mailbox[0]["message"]["source"], "worker");
    assert_eq!(mailbox[0]["message"]["sender"], run.worker_id);
    assert_eq!(mailbox[0]["message"]["taskId"], run.task_id);
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "claimed"
    );
}

#[test]
fn member_lost_reply_reuses_committed_operation_and_revocation_blocks_cached_results() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    let send = member_operation(
        &run,
        "send",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"持久请求"}),
    );
    let first = f.store.member_call(&binding, &send).unwrap();
    drop(f.store);
    f.store = Store::open(f.dir.path()).unwrap();
    assert_eq!(f.store.member_call(&binding, &send).unwrap(), first);
    assert_eq!(f.store.mailbox(None).unwrap().as_array().unwrap().len(), 1);
    let mut changed = send.clone();
    changed.input["body"] = json!("不同请求");
    assert!(matches!(
        f.store.member_call(&binding, &changed),
        Err(Error::Conflict(_))
    ));
    changed = send.clone();
    changed.operation_id = "new-id-for-same-call".into();
    assert!(matches!(
        f.store.member_call(&binding, &changed),
        Err(Error::Conflict(_))
    ));
    assert_eq!(f.store.mailbox(None).unwrap().as_array().unwrap().len(), 1);
    f.store
        .execute(
            "revoke-member",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 2,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(run.worker_id.clone(), vec![Permission::Communicate])]),
            },
        )
        .unwrap();
    assert!(f.store.member_call(&binding, &send).is_err());
    assert_eq!(f.store.mailbox(None).unwrap().as_array().unwrap().len(), 1);
}

#[test]
fn member_decision_and_call_ledger_rollback_together_then_retry_commits_once() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    let request = member_operation(
        &run,
        "clarify",
        "decision_request",
        json!({"revision":1,"handler":f.human,"question":"是否包含平局重开？","impact":"确定验收范围","options":["包含","不包含"]}),
    );
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_member_ledger BEFORE INSERT ON member_requests BEGIN SELECT RAISE(ABORT,'injected ledger failure'); END;").unwrap();
    assert!(f.store.member_call(&binding, &request).is_err());
    assert!(
        f.store
            .decisions(&run.task_id)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        f.store
            .mailbox(None)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    db.execute_batch("DROP TRIGGER fail_member_ledger;")
        .unwrap();
    let result = f.store.member_call(&binding, &request).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["decision"]["requester"], run.worker_id);
    assert_eq!(f.store.member_call(&binding, &request).unwrap(), result);
    assert_eq!(
        f.store
            .decisions(&run.task_id)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.store.mailbox(None).unwrap()[0]["message"]["source"],
        "core"
    );
    assert_eq!(
        f.store.mailbox(None).unwrap()[0]["message"]["sender"],
        run.worker_id
    );
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "claimed"
    );
}

#[test]
fn member_tool_budget_counts_rejected_calls_and_stale_epoch_cannot_read() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    for number in 0..100 {
        let operation = member_operation(&run, &number.to_string(), "not_installed", json!({}));
        let result = f.store.member_call(&binding, &operation).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(f.store.member_call(&binding, &operation).unwrap(), result);
    }
    let read = member_operation(&run, "over-limit", "task_read", json!({}));
    assert!(
        matches!(f.store.member_call(&binding, &read), Err(Error::Conflict(message)) if message.contains("额度"))
    );
    f.store.runtime_register("replacement", 456).unwrap();
    let previous = member_operation(&run, "0", "not_installed", json!({}));
    assert!(f.store.member_call(&binding, &previous).is_err());
    assert!(f.store.bind_member("replacement", &run.id).is_err());
}

// These integration tests use the production TypeScript pipe client in a real
// Node child. The fixture chooses calls deterministically; no model is invoked.
struct FaultingPipe<W> {
    inner: W,
    lose_tool_reply: bool,
}
impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for FaultingPipe<W> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.lose_tool_reply
            && bytes
                .windows(b"\"kind\":\"tool.result\"".len())
                .any(|part| part == b"\"kind\":\"tool.result\"")
        {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected lost result after core commit",
            )));
        }
        std::pin::Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn channel_fixture(mode: &str) {
    use atelier::channel::{Capabilities, ChannelConfiguration};
    use std::process::Stdio;
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    f.store
        .runtime_register("service", std::process::id())
        .unwrap();
    let run = f
        .store
        .runtime_claim(
            "service",
            created["delivery"]["deliveryId"].as_str().unwrap(),
            &config,
        )
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        root.join("adapters/milkie/dist/src/channel.js").is_file(),
        "先在 adapters/milkie 执行 npm ci 与 npm run build，再运行跨进程集成测试"
    );
    let mut child = tokio::process::Command::new("node")
        .arg(root.join("tests/fixtures/member-channel.mjs"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    f.store
        .runtime_child_started("service", &run.id, pid, "owned-node-transport-fixture")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    let scope = f.store.member_scope(&binding).unwrap();
    let database = Database::open(f.dir.path().to_path_buf(), 8).await.unwrap();
    let skill = "只处理绑定投递，终态不等于业务完成";
    let operation = member_operation(
        &run,
        "send",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"真实管道提交、重复请求一次"}),
    );
    let (control, stop) = tokio::sync::watch::channel(false);
    let watcher = if mode == "blocker" {
        let control = control.clone();
        let client = database.client();
        let id = run.id.clone();
        Some(tokio::spawn(async move {
            loop {
                let id = id.clone();
                if client
                    .call(move |store| Ok(store.run(&id)?.stop_requested))
                    .await
                    .unwrap()
                {
                    control.send(true).unwrap();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }))
    } else {
        None
    };

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        atelier::channel::serve(
            child.stdout.take().unwrap(),
            FaultingPipe {
                inner: child.stdin.take().unwrap(),
                lose_tool_reply: mode == "lost_reply",
            },
            database.client(),
            binding.clone(),
            ChannelConfiguration {
                scope,
                capabilities: Capabilities::api(skill),
                start: json!({"skill":skill,"mode":mode,"operations":[operation,operation]}),
            },
            stop,
        ),
    )
    .await
    .unwrap();
    if let Some(watcher) = watcher {
        watcher.await.unwrap();
    }
    drop(control);
    let exit = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    database.close().await.unwrap();
    if mode == "normal" || mode == "respond" || mode == "blocker" {
        assert!(exit.success());
        assert_eq!(
            result.unwrap().stop_reason,
            if mode == "blocker" {
                "cancelled"
            } else {
                "completed"
            }
        );
        if mode == "blocker" {
            assert!(f.store.run(&run.id).unwrap().stop_requested);
            assert_eq!(
                f.store
                    .blockers(&run.task_id)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
        assert_eq!(
            f.store.mailbox(None).unwrap().as_array().unwrap().len(),
            if mode == "respond" || mode == "blocker" {
                2
            } else {
                1
            }
        );
        assert_eq!(
            f.store.task(&run.task_id).unwrap().messages_used,
            if mode == "respond" { 3 } else { 2 }
        );
    } else if mode == "lost_reply" {
        assert!(result.is_err());
        let cached = f.store.member_call(&binding, &operation).unwrap();
        assert_eq!(cached["ok"], true);
        assert_eq!(f.store.mailbox(None).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(f.store.task(&run.task_id).unwrap().messages_used, 2);
    } else {
        assert!(result.is_err());
        assert!(
            f.store
                .mailbox(None)
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    // Even a valid model terminal cannot mark the original message handled.
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "claimed"
    );
    f.store
        .runtime_run_observed_stopped(
            "service",
            &run.id,
            "测试管道子进程已回收；没有模型、容器或其他执行资源",
        )
        .unwrap();
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        if mode == "respond" || mode == "blocker" {
            "handled"
        } else {
            "blocked"
        }
    );
    assert!(f.store.member_call(&binding, &operation).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn real_typescript_pipe_replays_tool_once_and_terminal_does_not_handle_delivery() {
    channel_fixture("normal").await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_typescript_pipe_rejects_cross_task_and_truncated_terminal() {
    channel_fixture("wrong_scope").await;
    channel_fixture("gap").await;
    channel_fixture("conflict").await;
    channel_fixture("truncated").await;
}

#[tokio::test(flavor = "current_thread")]
async fn broken_result_pipe_preserves_committed_effect_and_replay_identity() {
    channel_fixture("lost_reply").await;
}

#[tokio::test(flavor = "current_thread")]
async fn actual_child_disposition_is_handled_only_after_child_is_reaped() {
    channel_fixture("respond").await;
}

#[test]
fn member_handling_result_survives_terminal_loss_and_needs_observed_stop() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    let wait = member_operation(
        &run,
        "wait",
        "message_respond",
        json!({"kind":"wait","reason":"等本人提供规则","handler":f.human}),
    );
    let saved = f.store.member_call(&binding, &wait).unwrap();
    assert_eq!(saved["ok"], true);
    let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
    assert_eq!(mailbox[0]["status"], "claimed");
    assert_eq!(mailbox[0]["handlingResult"]["result"]["handler"], f.human);
    assert_eq!(f.store.member_call(&binding, &wait).unwrap(), saved);
    let later = member_operation(
        &run,
        "later",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"终局后的额外操作"}),
    );
    assert_eq!(f.store.member_call(&binding, &later).unwrap()["ok"], false);
    drop(f.store);
    f.store = Store::open(f.dir.path()).unwrap();
    f.store.runtime_register("replacement", 456).unwrap();
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "uncertain"
    );
    f.store
        .runtime_run_observed_stopped("replacement", &run.id, "测试注入：确认所有旧资源已停止")
        .unwrap();
    let receipt = &f.store.mailbox(Some(&run.worker_id)).unwrap()[0];
    assert_eq!(receipt["status"], "handled");
    assert_eq!(receipt["handlingResult"], mailbox[0]["handlingResult"]);
    assert_eq!(f.store.task(&run.task_id).unwrap().state, "pending");
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
}

#[test]
fn member_disposition_rejects_fake_references_and_rolls_back_with_request_ledger() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    for (index, input) in [
        json!({"kind":"wait","reason":"","handler":f.human}),
        json!({"kind":"wait","reason":"等执行者","handler":f.team.executor}),
        json!({"kind":"reply","messageId":"invented"}),
        json!({"kind":"decision","decisionId":"invented"}),
    ]
    .into_iter()
    .enumerate()
    {
        let operation = member_operation(&run, &format!("bad-{index}"), "message_respond", input);
        assert_eq!(
            f.store.member_call(&binding, &operation).unwrap()["ok"],
            false
        );
    }
    let request = member_operation(
        &run,
        "decision",
        "decision_request",
        json!({"revision":1,"handler":f.human,"question":"谁先手？","impact":"补齐规则","options":[]}),
    );
    let decided = f.store.member_call(&binding, &request).unwrap();
    let response = member_operation(
        &run,
        "respond",
        "message_respond",
        json!({"kind":"decision","decisionId":decided["data"]["decision"]["id"]}),
    );
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER disposition_ledger_failure BEFORE INSERT ON member_requests WHEN NEW.operation_id='op-respond' BEGIN SELECT RAISE(ABORT,'injected disposition ledger failure'); END;").unwrap();
    assert!(f.store.member_call(&binding, &response).is_err());
    assert!(f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["handlingResult"].is_null());
    db.execute_batch("DROP TRIGGER disposition_ledger_failure;")
        .unwrap();
    assert_eq!(
        f.store.member_call(&binding, &response).unwrap()["ok"],
        true
    );
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试注入：已停止")
        .unwrap();
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "handled"
    );
}

#[test]
fn cancelled_or_superseded_task_does_not_finalize_an_old_handling_result() {
    for cancel in [true, false] {
        let mut f = Fixture::new(true);
        let (run, binding) = f.running_member();
        let wait = member_operation(
            &run,
            "wait",
            "message_respond",
            json!({"kind":"wait","reason":"等补充规则","handler":f.human}),
        );
        assert_eq!(f.store.member_call(&binding, &wait).unwrap()["ok"], true);
        let command = if cancel {
            Command::TaskCancel {
                id: run.task_id.clone(),
                revision: 1,
                reason: "取消".into(),
            }
        } else {
            Command::TaskUpdate {
                decision_id: None,
                id: run.task_id.clone(),
                revision: 1,
                goal: Some("新版规则".into()),
                contract: ContractPatch::default(),
                refresh_team: false,
            }
        };
        f.store.execute("change", &command).unwrap();
        f.store
            .runtime_run_observed_stopped("service", &run.id, "测试注入：已停止")
            .unwrap();
        let receipt = &f.store.mailbox(Some(&run.worker_id)).unwrap()[0];
        assert_eq!(receipt["status"], "cancelled");
        assert!(!receipt["handlingResult"].is_null());
    }
}

fn generic_contract() -> ContractPatch {
    ContractPatch {
        inputs: Some("提供的规则".into()),
        delivery: Some("交付可使用结果".into()),
        verification: Some("独立核对规则".into()),
        ..Default::default()
    }
}

#[test]
fn digital_leader_acceptance_keeps_coordination_responsibility_until_explicit_result() {
    for respond in [false, true] {
        let mut f = Fixture::new(true);
        let (run, binding) = f.running_member_with_contract(Some(generic_contract()));
        let accept = member_operation(
            &run,
            "accept",
            "task_intake",
            json!({"revision":2,"decision":"accept","reason":"约束和职责齐备"}),
        );
        let accepted = f.store.member_call(&binding, &accept).unwrap();
        assert_eq!(accepted["ok"], true);
        assert_eq!(accepted["data"]["task"]["state"], "active");
        assert_eq!(accepted["data"]["task"]["owner"], run.worker_id);
        assert!(accepted["data"]["delivery"].is_null());
        assert_eq!(f.store.member_call(&binding, &accept).unwrap(), accepted);
        let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
        let receipt = mailbox
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap();
        assert_eq!(receipt["status"], "claimed");
        assert!(receipt["handlingResult"].is_null());
        assert!(
            !mailbox
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["status"] == "queued")
        );
        if respond {
            let waiting = member_operation(
                &run,
                "wait",
                "message_respond",
                json!({"kind":"wait","reason":"等待本人确认输出格式后安排","handler":f.human}),
            );
            assert_eq!(f.store.member_call(&binding, &waiting).unwrap()["ok"], true);
        }
        f.store
            .runtime_run_observed_stopped("service", &run.id, "测试注入：所有资源已停止")
            .unwrap();
        let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
        let receipt = mailbox
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap();
        assert_eq!(
            receipt["status"],
            if respond { "handled" } else { "blocked" }
        );
        assert_eq!(f.store.task(&run.task_id).unwrap().state, "active");
        assert_eq!(f.store.task(&run.task_id).unwrap().revision, 3);
    }
}

#[test]
fn digital_intake_wait_and_decline_record_outcome_but_wait_for_resource_stop() {
    for decision in ["wait", "decline"] {
        let mut f = Fixture::new(true);
        let (run, binding) = f.running_member();
        let intake = member_operation(
            &run,
            "intake",
            "task_intake",
            json!({"revision":1,"decision":decision,"reason":"前置不满足"}),
        );
        let result = f.store.member_call(&binding, &intake).unwrap();
        assert_eq!(result["ok"], true);
        let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
        assert_eq!(mailbox[0]["status"], "claimed");
        assert_eq!(mailbox[0]["handlingResult"]["result"]["decision"], decision);
        assert_eq!(
            f.store.mailbox(None).unwrap()[0]["message"]["kind"],
            "result"
        );
        f.store
            .runtime_run_observed_stopped("service", &run.id, "测试注入：确认停止")
            .unwrap();
        assert_eq!(
            f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
            "handled"
        );
        assert_eq!(
            f.store.task(&run.task_id).unwrap().state,
            if decision == "wait" {
                "pending"
            } else {
                "closed"
            }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn digital_code_intake_validates_immutable_files_outside_database_before_publication() {
    use atelier::{content::GitInput, profile::VerificationProfile};
    let mut f = Fixture::new(true);
    let sample = tempfile::tempdir().unwrap();
    let prepared = atelier::sample::prepare(sample.path()).unwrap();
    let input: GitInput = serde_json::from_value(
        f.store
            .execute(
                "input",
                &Command::InputImport {
                    repository: sample.path().to_str().unwrap().into(),
                    commit: prepared["commit"].as_str().unwrap().into(),
                },
            )
            .unwrap(),
    )
    .unwrap();
    let profile = f
        .store
        .execute(
            "profile",
            &Command::ProfileImport {
                specification: VerificationProfile {
                    name: "测试固定检查配置".into(),
                    check_id: "rules".into(),
                    image: format!("sha256:{}", "a".repeat(64)),
                    argv: vec!["node".into(), "/checks/check.mjs".into()],
                },
            },
        )
        .unwrap();
    let (run, binding) = f.running_member_with_contract(Some(ContractPatch {
        code_input: Some(input.id.clone()),
        verification_profile: Some(profile["id"].as_str().unwrap().into()),
        ..generic_contract()
    }));
    let accept = member_operation(
        &run,
        "accept",
        "task_intake",
        json!({"revision":2,"decision":"accept","reason":"输入已核对"}),
    );
    assert!(matches!(
        f.store.member_call(&binding, &accept),
        Err(Error::Unavailable(_))
    ));
    let blob = f
        .dir
        .path()
        .join("objects/blobs")
        .join(&input.files[0].sha256);
    let original = std::fs::read(&blob).unwrap();
    std::fs::write(&blob, b"corrupted").unwrap();
    let database = Database::open(f.dir.path().to_path_buf(), 8).await.unwrap();
    assert!(
        database
            .client()
            .member_call(binding.clone(), accept.clone())
            .await
            .is_err()
    );
    assert_eq!(f.store.task(&run.task_id).unwrap().state, "pending");
    std::fs::write(&blob, original).unwrap();
    drop(sample);
    let result = database
        .client()
        .member_call(binding.clone(), accept.clone())
        .await
        .unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["data"]["task"]["state"], "active");
    // Replaying a committed acceptance does not re-read an external input.
    std::fs::remove_file(&blob).unwrap();
    assert_eq!(
        database
            .client()
            .member_call(binding, accept)
            .await
            .unwrap(),
        result
    );
    database.close().await.unwrap();
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
}

#[test]
fn ordinary_coordination_requires_a_real_reply_to_the_original_sender() {
    let mut f = Fixture::new(true);
    let (first, binding) = f.running_member();
    let wait = member_operation(
        &first,
        "wait",
        "message_respond",
        json!({"kind":"wait","reason":"等待新的工作信息","handler":first.worker_id}),
    );
    f.store.member_call(&binding, &wait).unwrap();
    f.store
        .runtime_run_observed_stopped("service", &first.id, "测试注入：前一轮停止")
        .unwrap();
    let incoming = f
        .store
        .execute(
            "human-note",
            &Command::MessageSend {
                task_id: first.task_id.clone(),
                recipient: first.worker_id.clone(),
                kind: "work.question".into(),
                body: "是否已经收到规则？".into(),
                reply_to: None,
            },
        )
        .unwrap();
    let run = f
        .store
        .runtime_claim(
            "service",
            incoming["deliveryId"].as_str().unwrap(),
            &first.configuration_id,
        )
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    f.store
        .runtime_child_started("service", &run.id, 322, "fixture-second-child")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    let unrelated = member_operation(
        &run,
        "unrelated",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"与原问题无引用的消息"}),
    );
    let unrelated = f.store.member_call(&binding, &unrelated).unwrap();
    let false_reply = member_operation(
        &run,
        "false-reply",
        "message_respond",
        json!({"kind":"reply","messageId":unrelated["data"]["messageId"]}),
    );
    assert_eq!(
        f.store.member_call(&binding, &false_reply).unwrap()["ok"],
        false
    );
    let reply = member_operation(
        &run,
        "reply",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"规则已收到","replyTo":incoming["messageId"]}),
    );
    let sent = f.store.member_call(&binding, &reply).unwrap();
    let respond = member_operation(
        &run,
        "respond",
        "message_respond",
        json!({"kind":"reply","messageId":sent["data"]["messageId"]}),
    );
    assert_eq!(f.store.member_call(&binding, &respond).unwrap()["ok"], true);
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试注入：第二轮停止")
        .unwrap();
    let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
    let receipt = mailbox
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == run.delivery_id)
        .unwrap();
    assert_eq!(receipt["status"], "handled");
    assert_eq!(
        receipt["handlingResult"]["result"]["messageId"],
        sent["data"]["messageId"]
    );
}

impl Fixture {
    fn claim_member_message(
        &mut self,
        delivery: &str,
        configuration: &str,
    ) -> (Run, atelier::member::MemberBinding) {
        let run = self
            .store
            .runtime_claim("service", delivery, configuration)
            .unwrap();
        self.store.runtime_begin_launch("service", &run.id).unwrap();
        let run = self
            .store
            .runtime_child_started("service", &run.id, 324, "fixture-later-child")
            .unwrap();
        let binding = self.store.bind_member("service", &run.id).unwrap();
        (run, binding)
    }
    fn answered_member_decision(&mut self) -> (Run, atelier::member::MemberBinding, String) {
        let (first, binding) = self.running_member();
        let ask = member_operation(
            &first,
            "ask",
            "decision_request",
            json!({"revision":1,"handler":self.human,"question":"请确认游戏规则","impact":"补齐契约","options":[]}),
        );
        let asked = self.store.member_call(&binding, &ask).unwrap();
        let id = asked["data"]["decision"]["id"].as_str().unwrap().to_owned();
        let respond = member_operation(
            &first,
            "waiting-answer",
            "message_respond",
            json!({"kind":"decision","decisionId":id}),
        );
        assert_eq!(
            self.store.member_call(&binding, &respond).unwrap()["ok"],
            true
        );
        self.store
            .runtime_run_observed_stopped("service", &first.id, "测试注入：提问后停止")
            .unwrap();
        let answer = self
            .store
            .execute(
                "human-answer",
                &Command::DecisionRespond {
                    id: id.clone(),
                    revision: 1,
                    answer: "X 先手，三子连线获胜，满盘未获胜为平局".into(),
                },
            )
            .unwrap();
        let (run, binding) = self.claim_member_message(
            answer["delivery"]["deliveryId"].as_str().unwrap(),
            &first.configuration_id,
        );
        (run, binding, id)
    }
}

#[test]
fn member_clarification_updates_contract_records_actual_effect_then_accepts() {
    let mut f = Fixture::new(true);
    let (run, binding, id) = f.answered_member_decision();
    let read = member_operation(&run, "read-decision", "decision_read", json!({"id":id}));
    let decision = f.store.member_call(&binding, &read).unwrap();
    assert_eq!(decision["data"]["state"], "responded");
    assert!(
        decision["data"]["answer"]
            .as_str()
            .unwrap()
            .contains("X 先手")
    );
    let update = member_operation(
        &run,
        "update",
        "task_update",
        json!({"revision":1,"inputs":"已确认的井字棋规则","delivery":"离线双人网页游戏","verification":"独立检查横纵及对角线与平局","decisionId":id}),
    );
    let updated = f.store.member_call(&binding, &update).unwrap();
    assert_eq!(updated["ok"], true);
    assert_eq!(updated["data"]["task"]["revision"], 2);
    assert!(updated["data"]["delivery"].is_null());
    assert_eq!(f.store.member_call(&binding, &update).unwrap(), updated);
    let current = f.store.decision(&id).unwrap();
    assert_eq!(current.state, "responded");
    assert_eq!(current.effective_revision, 2);
    assert_eq!(
        json!(current.operation),
        updated["data"]["operationReference"]
    );
    let lie = member_operation(
        &run,
        "no-change",
        "decision_record",
        json!({"id":id,"revision":current.revision,"resolution":{"kind":"no_change","reason":"其实没有修改"}}),
    );
    assert_eq!(f.store.member_call(&binding, &lie).unwrap()["ok"], false);
    let record = member_operation(
        &run,
        "record",
        "decision_record",
        json!({"id":id,"revision":current.revision,"resolution":{"kind":"operation","reference":updated["data"]["operationReference"]}}),
    );
    let result = f.store.member_call(&binding, &record).unwrap();
    assert_eq!(result["data"]["state"], "resolved");
    assert_eq!(
        f.store
            .mailbox(Some(&run.worker_id))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap()["status"],
        "claimed"
    );
    let accept = member_operation(
        &run,
        "accept",
        "task_intake",
        json!({"revision":2,"decision":"accept","reason":"规则及契约齐备"}),
    );
    assert_eq!(f.store.member_call(&binding, &accept).unwrap()["ok"], true);
    let late_update = member_operation(
        &run,
        "late-update",
        "task_update",
        json!({"revision":3,"delivery":"不能覆盖已承接契约"}),
    );
    assert_eq!(
        f.store.member_call(&binding, &late_update).unwrap()["ok"],
        false
    );
    let wait = member_operation(
        &run,
        "next-responsibility",
        "message_respond",
        json!({"kind":"wait","reason":"等待实际执行环境就绪","handler":f.human}),
    );
    assert_eq!(f.store.member_call(&binding, &wait).unwrap()["ok"], true);
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试注入：协调已结束")
        .unwrap();
    let task = f.store.task(&run.task_id).unwrap();
    assert_eq!(task.state, "active");
    assert_eq!(task.runs_used, 2);
    let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
    assert!(
        !mailbox
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["status"] == "queued")
    );
    assert_eq!(
        mailbox
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap()["status"],
        "handled"
    );
}

#[test]
fn member_contract_update_and_both_operation_ledgers_rollback_together() {
    let mut f = Fixture::new(true);
    let (run, binding, id) = f.answered_member_decision();
    let update = member_operation(
        &run,
        "update",
        "task_update",
        json!({"revision":1,"inputs":"已确认规则","decisionId":id}),
    );
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_member_update BEFORE INSERT ON member_requests WHEN NEW.operation_id='op-update' BEGIN SELECT RAISE(ABORT,'injected member update ledger failure'); END;").unwrap();
    assert!(f.store.member_call(&binding, &update).is_err());
    assert_eq!(f.store.task(&run.task_id).unwrap().revision, 1);
    assert_eq!(f.store.run(&run.id).unwrap().task_revision, 1);
    assert!(f.store.decision(&id).unwrap().operation.is_none());
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM requests WHERE actor=?1",
            [&run.worker_id],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER fail_member_update;")
        .unwrap();
    assert_eq!(f.store.member_call(&binding, &update).unwrap()["ok"], true);
    assert_eq!(f.store.task(&run.task_id).unwrap().revision, 2);
    assert_eq!(f.store.run(&run.id).unwrap().task_revision, 2);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM requests WHERE actor=?1",
            [&run.worker_id],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn blocked_decision_requires_waiting_owner_and_does_not_forge_resolution() {
    let mut f = Fixture::new(true);
    let (run, binding, id) = f.answered_member_decision();
    let record = member_operation(
        &run,
        "record",
        "decision_record",
        json!({"id":id,"revision":2,"resolution":{"kind":"blocked","reason":"缺少提供的输入文件"}}),
    );
    assert_eq!(
        f.store.member_call(&binding, &record).unwrap()["data"]["state"],
        "responded"
    );
    let pretend = member_operation(
        &run,
        "pretend-complete",
        "message_respond",
        json!({"kind":"decision","decisionId":id}),
    );
    assert_eq!(
        f.store.member_call(&binding, &pretend).unwrap()["ok"],
        false
    );
    let wait = member_operation(
        &run,
        "wait-input",
        "message_respond",
        json!({"kind":"wait","reason":"请提供输入文件后再继续","handler":f.human}),
    );
    assert_eq!(f.store.member_call(&binding, &wait).unwrap()["ok"], true);
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试注入：等待输入")
        .unwrap();
    assert_eq!(f.store.decision(&id).unwrap().state, "responded");
    let mailbox = f.store.mailbox(None).unwrap();
    assert!(
        mailbox
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["status"] == "queued" && r["message"]["kind"] == "result")
    );
    let supplied = f
        .store
        .execute(
            "human-supplied",
            &Command::MessageSend {
                task_id: run.task_id.clone(),
                recipient: run.worker_id.clone(),
                kind: "work.note".into(),
                body: "已提供所需规则说明".into(),
                reply_to: None,
            },
        )
        .unwrap();
    let (next, bound) = f.claim_member_message(
        supplied["deliveryId"].as_str().unwrap(),
        &run.configuration_id,
    );
    let update = member_operation(
        &next,
        "update-after-wait",
        "task_update",
        json!({"revision":1,"inputs":"补充后的规则说明","decisionId":id}),
    );
    let updated = f.store.member_call(&bound, &update).unwrap();
    assert_eq!(updated["ok"], true);
    let record = member_operation(
        &next,
        "resolved-after-wait",
        "decision_record",
        json!({"id":id,"revision":f.store.decision(&id).unwrap().revision,"resolution":{"kind":"operation","reference":updated["data"]["operationReference"]}}),
    );
    assert_eq!(
        f.store.member_call(&bound, &record).unwrap()["data"]["state"],
        "resolved"
    );
    let responded = member_operation(
        &next,
        "response-after-wait",
        "message_respond",
        json!({"kind":"decision","decisionId":id}),
    );
    assert_eq!(f.store.member_call(&bound, &responded).unwrap()["ok"], true);
    f.store
        .runtime_run_observed_stopped("service", &next.id, "测试注入：后续消息继续处理完成")
        .unwrap();
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 3);
}

#[test]
fn member_update_cannot_change_frozen_role_configuration_or_read_other_task_decisions() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    for (index, input) in [
        json!({"revision":1,"refreshTeam":true}),
        json!({"revision":1,"codeInput":"other-input"}),
        json!({"revision":1,"maxRuns":999}),
        json!({"revision":1,"actor":f.human}),
    ]
    .into_iter()
    .enumerate()
    {
        let operation =
            member_operation(&run, &format!("bad-update-{index}"), "task_update", input);
        assert_eq!(
            f.store.member_call(&binding, &operation).unwrap()["ok"],
            false
        );
    }
    let other = f
        .store
        .execute(
            "other-task",
            &Command::TaskCreate {
                team_id: f.team.id.clone(),
                goal: "其他任务".into(),
            },
        )
        .unwrap();
    let decision = f
        .store
        .execute(
            "other-decision",
            &Command::DecisionRequest {
                task_id: other["task"]["id"].as_str().unwrap().into(),
                revision: 1,
                handler: run.worker_id.clone(),
                question: "另一个任务的问题".into(),
                impact: "验证范围隔离".into(),
                options: vec![],
            },
        )
        .unwrap();
    let id = &decision["decision"]["id"];
    for (index, name, input) in [
        (0, "decision_read", json!({"id":id})),
        (
            1,
            "decision_respond",
            json!({"id":id,"revision":1,"answer":"越界回答"}),
        ),
    ] {
        let operation = member_operation(&run, &format!("cross-task-{index}"), name, input);
        let result = f.store.member_call(&binding, &operation).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "forbidden");
    }
    assert_eq!(f.store.task(&run.task_id).unwrap().revision, 1);
    assert_eq!(
        f.store.decision(id.as_str().unwrap()).unwrap().state,
        "open"
    );
}

#[test]
fn digital_decision_answer_requires_its_handler_and_waits_for_resource_stop() {
    let mut f = Fixture::new(true);
    let (first, binding) = f.running_member();
    let asked = f
        .store
        .execute(
            "human-question",
            &Command::DecisionRequest {
                task_id: first.task_id.clone(),
                revision: 1,
                handler: first.worker_id.clone(),
                question: "规则是否足够明确？".into(),
                impact: "确认准备情况".into(),
                options: vec!["是".into(), "否".into()],
            },
        )
        .unwrap();
    let id = asked["decision"]["id"].as_str().unwrap();
    let other = f
        .store
        .execute(
            "human-only-question",
            &Command::DecisionRequest {
                task_id: first.task_id.clone(),
                revision: 1,
                handler: f.human.clone(),
                question: "仅由本人回答".into(),
                impact: "测试权限".into(),
                options: vec![],
            },
        )
        .unwrap();
    let wrong = member_operation(
        &first,
        "wrong-delivery",
        "decision_respond",
        json!({"id":other["decision"]["id"],"revision":1,"answer":"否"}),
    );
    assert_eq!(f.store.member_call(&binding, &wrong).unwrap()["ok"], false);
    let wait = member_operation(
        &first,
        "wait",
        "message_respond",
        json!({"kind":"wait","reason":"随后处理正式问题","handler":first.worker_id}),
    );
    f.store.member_call(&binding, &wait).unwrap();
    f.store
        .runtime_run_observed_stopped("service", &first.id, "测试注入：首轮已停止")
        .unwrap();
    let (run, binding) = f.claim_member_message(
        asked["delivery"]["deliveryId"].as_str().unwrap(),
        &first.configuration_id,
    );
    let answer = member_operation(
        &run,
        "answer",
        "decision_respond",
        json!({"id":id,"revision":1,"answer":"否"}),
    );
    assert_eq!(
        f.store.member_call(&binding, &answer).unwrap()["data"]["decision"]["state"],
        "responded"
    );
    let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
    assert_eq!(
        mailbox
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap()["status"],
        "claimed"
    );
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试注入：回答后停止")
        .unwrap();
    let mailbox = f.store.mailbox(Some(&run.worker_id)).unwrap();
    assert_eq!(
        mailbox
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap()["status"],
        "handled"
    );
    assert_eq!(f.store.task(&run.task_id).unwrap().state, "pending");
}

impl Fixture {
    fn prepare_executor(&mut self) -> String {
        use atelier::connection::{ApiProtocol, ConnectionSpec};
        let connection = self
            .store
            .execute(
                "executor-connection",
                &Command::ConnectionCreate {
                    name: "执行测试配置".into(),
                    specification: ConnectionSpec::Api {
                        protocol: ApiProtocol::OpenaiChatCompletions,
                        model: "fixture".into(),
                        base_url: Some("https://example.invalid/v1".into()),
                    },
                },
            )
            .unwrap();
        let executor = self.team.executor.clone().unwrap();
        let worker = self
            .store
            .execute(
                "executor-config",
                &Command::WorkerUpdate {
                    id: executor.clone(),
                    revision: 1,
                    name: None,
                    description: None,
                    connection: Some(connection["connection"]["id"].as_str().unwrap().into()),
                    clear_connection: false,
                },
            )
            .unwrap();
        self.store
            .execute(
                "executor-grants",
                &Command::PermissionsUpdate {
                    decision_id: None,
                    team_id: self.team.id.clone(),
                    revision: self.store.team(&self.team.id).unwrap().revision,
                    grant: BTreeMap::from([(
                        executor,
                        vec![Permission::Execute, Permission::Communicate],
                    )]),
                    revoke: BTreeMap::new(),
                },
            )
            .unwrap();
        worker["execution_config"].as_str().unwrap().into()
    }
    fn accepted_human_task(&mut self) -> Task {
        let created = self.create();
        let id = created["task"]["id"].as_str().unwrap();
        self.complete_contract(id);
        self.store
            .execute(
                "accept",
                &Command::Intake {
                    id: id.into(),
                    revision: 2,
                    decision: IntakeDecision::Accept,
                    reason: "契约齐备".into(),
                },
            )
            .unwrap();
        self.store.task(id).unwrap()
    }
}

#[test]
fn execution_arrangement_is_persistent_unique_and_does_not_launch_or_spend_run_budget() {
    let mut f = Fixture::new(false);
    let configuration = f.prepare_executor();
    let task = f.accepted_human_task();
    let command = Command::TaskExecute {
        id: task.id.clone(),
        revision: task.revision,
        instruction: "按冻结契约完成交付".into(),
    };
    let arranged = f.store.execute("arrange", &command).unwrap();
    assert_eq!(arranged["delivery"]["status"], "queued");
    assert_eq!(f.store.execute("arrange", &command).unwrap(), arranged);
    assert!(matches!(
        f.store.execute("duplicate", &command),
        Err(Error::Conflict(_))
    ));
    assert_eq!(f.store.task(&task.id).unwrap().runs_used, 0);
    assert_eq!(
        f.store.task(&task.id).unwrap().messages_used,
        task.messages_used + 1
    );
    let executor = task.team_snapshot.executor.as_ref().unwrap();
    f.store = Store::open(f.dir.path()).unwrap();
    let mailbox = f.store.mailbox(Some(executor)).unwrap();
    assert_eq!(mailbox.as_array().unwrap().len(), 1);
    assert_eq!(mailbox[0]["message"]["kind"], "assignment.execute");
    assert_eq!(mailbox[0]["message"]["sender"], f.human);
    assert_eq!(mailbox[0]["status"], "queued");
    f.store.runtime_register("service", 123).unwrap();
    let run = f
        .store
        .runtime_claim(
            "service",
            arranged["delivery"]["deliveryId"].as_str().unwrap(),
            &configuration,
        )
        .unwrap();
    assert_eq!(run.purpose, "execute");
    assert_eq!(f.store.task(&task.id).unwrap().runs_used, 1);
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试：未启动资源")
        .unwrap();
    assert!(matches!(
        f.store.execute("new-execute-after-failure", &command),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn digital_leader_can_arrange_then_explicitly_finish_its_coordination_receipt() {
    let mut f = Fixture::new(true);
    f.prepare_executor();
    let (run, binding) = f.running_member_with_contract(Some(generic_contract()));
    assert!(
        f.store.member_description(&binding).unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "task_arrange")
    );
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "accept",
                    "task_intake",
                    json!({"revision":2,"decision":"accept","reason":"资料齐备"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let operation = member_operation(
        &run,
        "arrange",
        "task_arrange",
        json!({"revision":3,"action":"execute","instruction":"按冻结契约执行"}),
    );
    let arranged = f.store.member_call(&binding, &operation).unwrap();
    assert_eq!(arranged["ok"], true, "{arranged}");
    assert_eq!(arranged["data"]["delivery"]["status"], "queued");
    assert_eq!(f.store.member_call(&binding, &operation).unwrap(), arranged);
    let duplicate = member_operation(
        &run,
        "other-arrange",
        "task_arrange",
        operation.input.clone(),
    );
    assert_eq!(
        f.store.member_call(&binding, &duplicate).unwrap()["ok"],
        false
    );
    let read = f
        .store
        .member_call(
            &binding,
            &member_operation(&run, "read", "task_read", json!({})),
        )
        .unwrap();
    assert_eq!(
        read["data"]["assignments"][0]["messageId"],
        arranged["data"]["delivery"]["messageId"]
    );
    let receipt = f.store.mailbox(Some(&run.worker_id)).unwrap();
    assert_eq!(
        receipt
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap()["status"],
        "claimed"
    );
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "invented",
                    "message_respond",
                    json!({"kind":"assignment","messageId":"not-an-assignment"})
                )
            )
            .unwrap()["ok"],
        false
    );
    assert_eq!(f.store.member_call(&binding,&member_operation(&run,"finish","message_respond",json!({"kind":"assignment","messageId":arranged["data"]["delivery"]["messageId"]}))).unwrap()["ok"],true);
    f.store
        .runtime_run_observed_stopped("service", &run.id, "测试：成员已停止")
        .unwrap();
    let receipt = f.store.mailbox(Some(&run.worker_id)).unwrap();
    assert_eq!(
        receipt
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.delivery_id)
            .unwrap()["status"],
        "handled"
    );
    assert_eq!(
        f.store.mailbox(f.team.executor.as_deref()).unwrap()[0]["status"],
        "queued"
    );
}

#[test]
fn blocked_arrangement_retains_reason_and_requires_waiting_owner() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member_with_contract(Some(generic_contract()));
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "accept",
                    "task_intake",
                    json!({"revision":2,"decision":"accept","reason":"已知配置仍待补充"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let arranged = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "arrange",
                "task_arrange",
                json!({"revision":3,"action":"execute","instruction":"完成交付"}),
            ),
        )
        .unwrap();
    assert_eq!(arranged["ok"], true);
    assert_eq!(arranged["data"]["delivery"]["status"], "blocked");
    assert!(
        arranged["data"]["delivery"]["reason"]
            .as_str()
            .unwrap()
            .contains("授权")
    );
    assert_eq!(f.store.member_call(&binding,&member_operation(&run,"finish","message_respond",json!({"kind":"assignment","messageId":arranged["data"]["delivery"]["messageId"]}))).unwrap()["ok"],false);
    assert_eq!(f.store.member_call(&binding,&member_operation(&run,"wait","message_respond",json!({"kind":"wait","reason":"执行成员冻结配置和授权缺失，需要本人处理","handler":f.human}))).unwrap()["ok"],true);
}

#[test]
fn execution_arrangement_rechecks_authority_and_rolls_back_with_ledger_failure() {
    let mut f = Fixture::new(false);
    f.prepare_executor();
    let task = f.accepted_human_task();
    let command = Command::TaskExecute {
        id: task.id.clone(),
        revision: task.revision,
        instruction: "执行".into(),
    };
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_arrangement BEFORE INSERT ON requests WHEN NEW.id='arrange' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(matches!(
        f.store.execute("arrange", &command),
        Err(Error::Database(_))
    ));
    assert!(
        f.store
            .mailbox(f.team.executor.as_deref())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store.task(&task.id).unwrap().messages_used,
        task.messages_used
    );
    db.execute_batch("DROP TRIGGER reject_arrangement;")
        .unwrap();
    let arranged = f.store.execute("arrange", &command).unwrap();
    assert_eq!(arranged["delivery"]["status"], "queued");
    f.store
        .execute(
            "revoke",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: f.store.team(&f.team.id).unwrap().revision,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(f.human.clone(), vec![Permission::Arrange])]),
            },
        )
        .unwrap();
    assert!(matches!(
        f.store.execute("arrange", &command),
        Err(Error::Forbidden(_))
    ));
}

#[test]
fn administrator_cannot_arrange_for_digital_leader_and_pending_cannot_execute() {
    let mut f = Fixture::new(true);
    let task = f.create();
    assert!(matches!(
        f.store.execute(
            "arrange",
            &Command::TaskExecute {
                id: task["task"]["id"].as_str().unwrap().into(),
                revision: 1,
                instruction: "替代数字负责人".into()
            }
        ),
        Err(Error::Forbidden(_))
    ));
    let mut f = Fixture::new(false);
    let task = f.create();
    assert!(matches!(
        f.store.execute(
            "arrange",
            &Command::TaskExecute {
                id: task["task"]["id"].as_str().unwrap().into(),
                revision: 1,
                instruction: "未承接就执行".into()
            }
        ),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn concurrent_distinct_requests_create_only_one_execution_arrangement() {
    let mut f = Fixture::new(false);
    f.prepare_executor();
    let task = f.accepted_human_task();
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for n in 0..2 {
        let path = f.dir.path().to_path_buf();
        let barrier = barrier.clone();
        let id = task.id.clone();
        let revision = task.revision;
        handles.push(std::thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            barrier.wait();
            store.execute(
                &format!("arrange-{n}"),
                &Command::TaskExecute {
                    id,
                    revision,
                    instruction: "两个客户端同时安排".into(),
                },
            )
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(
        f.store
            .mailbox(f.team.executor.as_deref())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let actual = f.store.task(&task.id).unwrap();
    assert_eq!(actual.messages_used, task.messages_used + 1);
    assert_eq!(actual.runs_used, 0);
}

impl Fixture {
    fn running_executor(&mut self) -> (Run, atelier::member::MemberBinding, std::path::PathBuf) {
        let config = self.prepare_executor();
        let task = self.accepted_human_task();
        let arranged = self
            .store
            .execute(
                "arrange",
                &Command::TaskExecute {
                    id: task.id.clone(),
                    revision: task.revision,
                    instruction: "完成当前任务".into(),
                },
            )
            .unwrap();
        self.store.runtime_register("service", 123).unwrap();
        let run = self
            .store
            .runtime_claim(
                "service",
                arranged["delivery"]["deliveryId"].as_str().unwrap(),
                &config,
            )
            .unwrap();
        let candidate = self
            .store
            .runtime_candidate_directory("service", &run.id)
            .unwrap();
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::write(candidate.join("index.html"), "<h1>公开合成产出</h1>").unwrap();
        self.store.runtime_begin_launch("service", &run.id).unwrap();
        let run = self
            .store
            .runtime_child_started("service", &run.id, 321, "fixture-execution")
            .unwrap();
        let binding = self.store.bind_member("service", &run.id).unwrap();
        (run, binding, candidate)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_submission_requires_fixed_content_and_stop_before_handled() {
    let mut f = Fixture::new(false);
    let (run, binding, candidate) = f.running_executor();
    let operation = member_operation(
        &run,
        "submit",
        "artifact_submit",
        json!({"summary":"已提供首页"}),
    );
    let submitted = f.store.member_call(&binding, &operation).unwrap();
    assert_eq!(submitted["ok"], true, "{submitted}");
    assert_eq!(submitted["data"]["state"], "submitted");
    assert_eq!(
        f.store.member_call(&binding, &operation).unwrap(),
        submitted
    );
    assert!(
        f.store
            .task(&run.task_id)
            .unwrap()
            .current_artifact
            .is_none()
    );
    assert!(f.store.artifact_for_run(&run.id).unwrap().is_none());
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "claimed"
    );
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "after-submit",
                    "message_send",
                    json!({"recipient":f.human,"kind":"work.note","body":"提交后继续写"})
                )
            )
            .unwrap()["ok"],
        false
    );
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let artifact = database
        .client()
        .runtime_fix_artifact(
            "service".into(),
            run.id.clone(),
            "测试注入：所有候选写入者及在途调用已停止".into(),
        )
        .await
        .unwrap();
    assert!(!artifact.partial);
    assert_eq!(artifact.worker_id, run.worker_id);
    assert_eq!(artifact.files[0].path, "index.html");
    assert_eq!(
        f.store
            .task(&run.task_id)
            .unwrap()
            .current_artifact
            .as_deref(),
        Some(artifact.id.as_str())
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "stopped");
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "handled"
    );
    assert!(
        f.store
            .mailbox(f.team.verifier.as_deref())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let query = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(f.dir.path())
        .args(["--json", "artifact", "show", &artifact.id])
        .output()
        .unwrap();
    assert!(query.status.success());
    let result: Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(result["data"]["id"], artifact.id);
    std::fs::remove_dir_all(candidate).unwrap();
    let replay = database
        .client()
        .runtime_fix_artifact("service".into(), run.id.clone(), "丢回复核对".into())
        .await
        .unwrap();
    assert_eq!(replay.id, artifact.id);
    let bytes = std::fs::read(
        f.dir
            .path()
            .join("objects/blobs")
            .join(&artifact.files[0].sha256),
    )
    .unwrap();
    assert_eq!(bytes, "<h1>公开合成产出</h1>".as_bytes());
    database.close().await.unwrap();
}

#[test]
fn artifact_publication_rolls_back_reference_message_and_stop_together() {
    let mut f = Fixture::new(false);
    let (run, binding, _) = f.running_executor();
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "submit",
                    "artifact_submit",
                    json!({"summary":"完成首页"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_artifact_result BEFORE INSERT ON messages WHEN NEW.event_key LIKE 'artifact:%' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let prepared = f
        .store
        .runtime_prepare_artifact("service", &run.id)
        .unwrap()
        .fix()
        .unwrap();
    assert!(matches!(
        f.store
            .runtime_publish_artifact("service", prepared, "测试资源停止"),
        Err(Error::Database(_))
    ));
    assert!(f.store.artifact_for_run(&run.id).unwrap().is_none());
    assert!(
        f.store
            .task(&run.task_id)
            .unwrap()
            .current_artifact
            .is_none()
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "running");
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "claimed"
    );
    db.execute_batch("DROP TRIGGER reject_artifact_result;")
        .unwrap();
    let prepared = f
        .store
        .runtime_prepare_artifact("service", &run.id)
        .unwrap()
        .fix()
        .unwrap();
    assert!(
        !f.store
            .runtime_publish_artifact("service", prepared, "重新核对停止")
            .unwrap()
            .partial
    );
}

#[test]
fn revoked_then_restored_execution_only_preserves_partial_history() {
    let mut f = Fixture::new(false);
    let (run, binding, _) = f.running_executor();
    f.store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"待固定"}),
            ),
        )
        .unwrap();
    let fixed = f
        .store
        .runtime_prepare_artifact("service", &run.id)
        .unwrap()
        .fix()
        .unwrap();
    for (request, revoking) in [("revoke", true), ("restore", false)] {
        let permissions = BTreeMap::from([(run.worker_id.clone(), vec![Permission::Execute])]);
        f.store
            .execute(
                request,
                &Command::PermissionsUpdate {
                    decision_id: None,
                    team_id: f.team.id.clone(),
                    revision: f.store.team(&f.team.id).unwrap().revision,
                    grant: if revoking {
                        BTreeMap::new()
                    } else {
                        permissions.clone()
                    },
                    revoke: if revoking {
                        permissions
                    } else {
                        BTreeMap::new()
                    },
                },
            )
            .unwrap();
    }
    let artifact = f
        .store
        .runtime_publish_artifact("service", fixed, "已核对撤权后的全部资源停止")
        .unwrap();
    assert!(artifact.partial);
    assert!(
        f.store
            .task(&run.task_id)
            .unwrap()
            .current_artifact
            .is_none()
    );
    assert!(f.store.run(&run.id).unwrap().authority_revoked);
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "blocked"
    );
}

#[test]
fn missing_submission_is_partial_and_cancellation_never_publishes_current_artifact() {
    for cancelled in [false, true] {
        let mut f = Fixture::new(false);
        let (run, _, _) = f.running_executor();
        if cancelled {
            f.store
                .execute(
                    "cancel",
                    &Command::TaskCancel {
                        id: run.task_id.clone(),
                        revision: run.task_revision,
                        reason: "停止任务".into(),
                    },
                )
                .unwrap();
        }
        let fixed = f
            .store
            .runtime_prepare_artifact("service", &run.id)
            .unwrap()
            .fix()
            .unwrap();
        let artifact = f
            .store
            .runtime_publish_artifact("service", fixed, "核对停止后的未提交内容")
            .unwrap();
        assert!(artifact.partial);
        let task = f.store.task(&run.task_id).unwrap();
        assert_eq!(task.current_artifact.is_none(), cancelled);
        assert_eq!(
            f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
            if cancelled { "cancelled" } else { "blocked" }
        );
        assert_eq!(task.state, if cancelled { "closed" } else { "active" });
    }
}

#[test]
fn artifact_fixed_after_lost_terminal_uses_saved_submission_without_new_model_run() {
    let mut f = Fixture::new(false);
    let (run, binding, _) = f.running_executor();
    f.store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"已提交，终态丢失"}),
            ),
        )
        .unwrap();
    let runs = f.store.task(&run.task_id).unwrap().runs_used;
    f.store.runtime_register("restarted", 456).unwrap();
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    let fixed = f
        .store
        .runtime_prepare_artifact("restarted", &run.id)
        .unwrap()
        .fix()
        .unwrap();
    let artifact = f
        .store
        .runtime_publish_artifact("restarted", fixed, "新服务已核对旧进程及关联资源全部停止")
        .unwrap();
    assert!(!artifact.partial);
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, runs);
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "handled"
    );
}

#[cfg(unix)]
#[test]
fn artifact_snapshot_rejects_links_special_files_and_oversized_content() {
    for mode in ["symlink", "hardlink", "socket", "oversized"] {
        let mut f = Fixture::new(false);
        let (run, _, candidate) = f.running_executor();
        let target = candidate.join("invalid");
        let socket_directory = tempfile::tempdir_in("/tmp").unwrap();
        let socket = match mode {
            "symlink" => {
                std::os::unix::fs::symlink("/etc/passwd", &target).unwrap();
                None
            }
            "hardlink" => {
                std::fs::hard_link(candidate.join("index.html"), &target).unwrap();
                None
            }
            "socket" => {
                let short_path = socket_directory.path().join("socket");
                let listener = std::os::unix::net::UnixListener::bind(&short_path).unwrap();
                std::fs::rename(short_path, &target).unwrap();
                Some(listener)
            }
            "oversized" => {
                std::fs::File::create(&target)
                    .unwrap()
                    .set_len(atelier::content::INPUT_LIMIT + 1)
                    .unwrap();
                None
            }
            _ => unreachable!(),
        };
        assert!(
            f.store
                .runtime_prepare_artifact("service", &run.id)
                .unwrap()
                .fix()
                .is_err(),
            "{mode}"
        );
        assert!(f.store.artifact_for_run(&run.id).unwrap().is_none());
        assert!(
            f.store
                .task(&run.task_id)
                .unwrap()
                .current_artifact
                .is_none()
        );
        drop(socket);
    }
}

#[test]
fn coordination_run_cannot_submit_or_select_an_artifact_directory() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member();
    let result = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"冒充执行"}),
            ),
        )
        .unwrap();
    assert_eq!(result["ok"], false);
    assert!(
        !f.store.member_description(&binding).unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "artifact_submit")
    );
    assert!(
        f.store
            .runtime_prepare_artifact("service", &run.id)
            .is_err()
    );
}

impl Fixture {
    fn prepare_verifier(&mut self) -> String {
        use atelier::connection::{ApiProtocol, ConnectionSpec};
        let connection = self
            .store
            .execute(
                "verifier-connection",
                &Command::ConnectionCreate {
                    name: "检验 fixture".into(),
                    specification: ConnectionSpec::Api {
                        protocol: ApiProtocol::OpenaiChatCompletions,
                        model: "fixture".into(),
                        base_url: Some("https://example.invalid/v1".into()),
                    },
                },
            )
            .unwrap();
        let worker = self.team.verifier.clone().unwrap();
        let configured = self
            .store
            .execute(
                "verifier-config",
                &Command::WorkerUpdate {
                    id: worker.clone(),
                    revision: 1,
                    name: None,
                    description: None,
                    connection: Some(connection["connection"]["id"].as_str().unwrap().into()),
                    clear_connection: false,
                },
            )
            .unwrap();
        self.store
            .execute(
                "verifier-grants",
                &Command::PermissionsUpdate {
                    decision_id: None,
                    team_id: self.team.id.clone(),
                    revision: self.store.team(&self.team.id).unwrap().revision,
                    grant: BTreeMap::from([(
                        worker,
                        vec![Permission::Verify, Permission::Communicate],
                    )]),
                    revoke: BTreeMap::new(),
                },
            )
            .unwrap();
        configured["execution_config"].as_str().unwrap().into()
    }
    fn grant_direct_handoff(&mut self) {
        self.store
            .execute(
                "handoff-grant",
                &Command::PermissionsUpdate {
                    decision_id: None,
                    team_id: self.team.id.clone(),
                    revision: self.store.team(&self.team.id).unwrap().revision,
                    grant: BTreeMap::from([(
                        self.team.executor.clone().unwrap(),
                        vec![Permission::Handoff],
                    )]),
                    revoke: BTreeMap::new(),
                },
            )
            .unwrap();
    }
    fn fix_run(&mut self, run: &Run) -> atelier::artifact::Artifact {
        let fixed = self
            .store
            .runtime_prepare_artifact("service", &run.id)
            .unwrap()
            .fix()
            .unwrap();
        self.store
            .runtime_publish_artifact("service", fixed, "测试注入：所有资源已停止")
            .unwrap()
    }
    fn claim_handoff(
        &mut self,
        id: &str,
        configuration: &str,
    ) -> (Run, atelier::member::MemberBinding) {
        let h = self.store.handoff(id).unwrap();
        let run = self
            .store
            .runtime_claim("service", &h.delivery_id, configuration)
            .unwrap();
        assert_eq!(run.purpose, "verify");
        self.store.runtime_begin_launch("service", &run.id).unwrap();
        let run = self
            .store
            .runtime_child_started("service", &run.id, 432, "fixture-verifier")
            .unwrap();
        let binding = self.store.bind_member("service", &run.id).unwrap();
        (run, binding)
    }
}

#[test]
fn direct_handoff_waits_for_artifact_fix_and_acceptance_does_not_finish_verification() {
    let mut f = Fixture::new(false);
    f.grant_direct_handoff();
    let configuration = f.prepare_verifier();
    let (run, binding, _) = f.running_executor();
    let submitted = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"完成首页","handoff":"请按冻结规则独立检查首页"}),
            ),
        )
        .unwrap();
    assert_eq!(submitted["ok"], true);
    assert!(
        f.store
            .mailbox(f.team.verifier.as_deref())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let artifact = f.fix_run(&run);
    assert!(!artifact.partial);
    assert!(artifact.handoff_error.is_none());
    let id = artifact.handoff_id.unwrap();
    let offered = f.store.handoff(&id).unwrap();
    assert_eq!(offered.sender, run.worker_id);
    assert_eq!(offered.artifact_id, artifact.id);
    assert_eq!(offered.state, "offered");
    assert_eq!(
        f.store.mailbox(f.team.verifier.as_deref()).unwrap()[0]["status"],
        "queued"
    );
    let (verify, bound) = f.claim_handoff(&id, &configuration);
    let accept = member_operation(
        &verify,
        "accept",
        "handoff_respond",
        json!({"id":id,"revision":1,"accept":true,"reason":"资料足够，开始独立检查"}),
    );
    let result = f.store.member_call(&bound, &accept).unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(f.store.member_call(&bound, &accept).unwrap(), result);
    assert_eq!(f.store.handoff(&id).unwrap().state, "accepted");
    assert!(f.store.mailbox(Some(&verify.worker_id)).unwrap()[0]["handlingResult"].is_null());
    f.store
        .runtime_run_observed_stopped("service", &verify.id, "没有提交检验证据即退出")
        .unwrap();
    assert_eq!(
        f.store.mailbox(Some(&verify.worker_id)).unwrap()[0]["status"],
        "blocked"
    );
    assert_eq!(f.store.task(&run.task_id).unwrap().state, "active");
}

#[test]
fn leader_handoff_cli_rejection_notifies_sender_and_allows_corrected_reoffer() {
    let mut f = Fixture::new(false);
    let configuration = f.prepare_verifier();
    let (run, binding, _) = f.running_executor();
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "forbidden-direct",
                    "artifact_submit",
                    json!({"summary":"完成","handoff":"无权直接送检"})
                )
            )
            .unwrap()["ok"],
        false
    );
    f.store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"完成首页，交由负责人决定"}),
            ),
        )
        .unwrap();
    let artifact = f.fix_run(&run);
    assert!(artifact.handoff_id.is_none());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(f.dir.path())
        .args([
            "--json",
            "--request-id",
            "verify",
            "task",
            "verify",
            &run.task_id,
            "--revision",
            &run.task_revision.to_string(),
            "--artifact",
            &artifact.id,
            "--instruction",
            "请检验首页",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = result["data"]["handoff"]["id"].as_str().unwrap();
    let duplicate = Command::TaskVerify {
        verification_id: None,
        blocker_id: None,
        id: run.task_id.clone(),
        revision: run.task_revision,
        artifact_id: artifact.id.clone(),
        instruction: "不同请求不能重复送检".into(),
    };
    assert!(matches!(
        f.store.execute("duplicate", &duplicate),
        Err(Error::Conflict(_))
    ));
    let (verify, bound) = f.claim_handoff(id, &configuration);
    let reject = member_operation(
        &verify,
        "reject",
        "handoff_respond",
        json!({"id":id,"revision":1,"accept":false,"reason":"缺少操作说明，请补充"}),
    );
    assert_eq!(f.store.member_call(&bound, &reject).unwrap()["ok"], true);
    assert_eq!(
        f.store.mailbox(Some(&verify.worker_id)).unwrap()[0]["status"],
        "claimed"
    );
    f.store
        .runtime_run_observed_stopped("service", &verify.id, "拒收后已确认停止")
        .unwrap();
    assert_eq!(
        f.store.mailbox(Some(&verify.worker_id)).unwrap()[0]["status"],
        "handled"
    );
    let messages = f.store.mailbox(Some(&f.human)).unwrap();
    assert!(messages.as_array().unwrap().iter().any(|r| {
        r["message"]["body"]
            .as_str()
            .is_some_and(|s| s.contains("缺少操作说明"))
    }));
    let corrected = f
        .store
        .execute(
            "corrected",
            &Command::TaskVerify {
                verification_id: None,
                blocker_id: None,
                instruction: "补充：首页直接打开，点击空格轮流落子".into(),
                id: run.task_id.clone(),
                revision: run.task_revision,
                artifact_id: artifact.id.clone(),
            },
        )
        .unwrap();
    assert_ne!(corrected["handoff"]["id"], id);
    assert_eq!(f.store.handoff(id).unwrap().state, "rejected");
}

#[test]
fn handoff_notification_failure_rolls_back_fixed_artifact_and_can_retry() {
    let mut f = Fixture::new(false);
    f.grant_direct_handoff();
    f.prepare_verifier();
    let (run, binding, _) = f.running_executor();
    f.store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"完成首页","handoff":"请检验"}),
            ),
        )
        .unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_handoff BEFORE INSERT ON messages WHEN NEW.kind='handoff.verify' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let prepared = f
        .store
        .runtime_prepare_artifact("service", &run.id)
        .unwrap()
        .fix()
        .unwrap();
    assert!(matches!(
        f.store
            .runtime_publish_artifact("service", prepared, "测试停止"),
        Err(Error::Database(_))
    ));
    assert!(
        f.store
            .task(&run.task_id)
            .unwrap()
            .current_artifact
            .is_none()
    );
    assert!(f.store.artifact_for_run(&run.id).unwrap().is_none());
    assert_eq!(f.store.run(&run.id).unwrap().state, "running");
    assert!(
        f.store
            .mailbox(f.team.verifier.as_deref())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    db.execute_batch("DROP TRIGGER reject_handoff;").unwrap();
    assert!(f.fix_run(&run).handoff_id.is_some());
}

#[test]
fn exhausted_message_budget_keeps_artifact_and_reports_failed_handoff_intent() {
    let mut f = Fixture::new(false);
    f.grant_direct_handoff();
    f.prepare_verifier();
    let (run, binding, _) = f.running_executor();
    let task = f.store.task(&run.task_id).unwrap();
    for n in task.messages_used..task.contract.max_messages {
        assert_eq!(
            f.store
                .member_call(
                    &binding,
                    &member_operation(
                        &run,
                        &format!("note-{n}"),
                        "message_send",
                        json!({"recipient":f.human,"kind":"work.note","body":"有界消息额度测试"})
                    )
                )
                .unwrap()["ok"],
            true
        );
    }
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "submit",
                    "artifact_submit",
                    json!({"summary":"完成首页","handoff":"请检验"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let artifact = f.fix_run(&run);
    assert!(!artifact.partial);
    assert!(artifact.handoff_id.is_none());
    assert!(artifact.handoff_error.unwrap().contains("额度耗尽"));
    assert!(
        f.store
            .mailbox(f.team.verifier.as_deref())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "handled"
    );
}

#[test]
fn handoff_rejects_partial_and_wrong_task_and_keeps_target_configuration_blocked() {
    let mut f = Fixture::new(false);
    let (run, _, _) = f.running_executor();
    let partial = f.fix_run(&run);
    assert!(matches!(
        f.store.execute(
            "partial-verify",
            &Command::TaskVerify {
                verification_id: None,
                blocker_id: None,
                id: run.task_id,
                revision: run.task_revision,
                artifact_id: partial.id,
                instruction: "不能以 partial 送交完整检验".into()
            }
        ),
        Err(Error::Conflict(_))
    ));
    let mut f = Fixture::new(false);
    let (run, binding, _) = f.running_executor();
    f.store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"已完成"}),
            ),
        )
        .unwrap();
    let artifact = f.fix_run(&run);
    let offered = f
        .store
        .execute(
            "verify",
            &Command::TaskVerify {
                verification_id: None,
                blocker_id: None,
                id: run.task_id.clone(),
                revision: run.task_revision,
                artifact_id: artifact.id.clone(),
                instruction: "缺少检验配置仍保留待办".into(),
            },
        )
        .unwrap();
    assert_eq!(offered["delivery"]["status"], "blocked");
    let other = f
        .store
        .execute(
            "other",
            &Command::TaskCreate {
                team_id: f.team.id.clone(),
                goal: "另一任务".into(),
            },
        )
        .unwrap();
    assert!(
        f.store
            .execute(
                "wrong-task",
                &Command::TaskVerify {
                    verification_id: None,
                    blocker_id: None,
                    id: other["task"]["id"].as_str().unwrap().into(),
                    revision: 1,
                    artifact_id: artifact.id,
                    instruction: "越界引用".into()
                }
            )
            .is_err()
    );
}

impl Fixture {
    fn prepare_code_execution(&mut self, image: &str) -> (Run, String) {
        let leader_config = (self.team.leader != self.human).then(|| self.prepare_digital_leader());
        self.grant_direct_handoff();
        let verifier = self.prepare_verifier();
        let executor = self.prepare_executor();
        let sample = tempfile::tempdir().unwrap();
        let sample_info = atelier::sample::prepare(sample.path()).unwrap();
        let input = self
            .store
            .execute(
                "input",
                &Command::InputImport {
                    repository: sample.path().to_str().unwrap().into(),
                    commit: sample_info["commit"].as_str().unwrap().into(),
                },
            )
            .unwrap();
        let profile = self
            .store
            .execute(
                "profile",
                &Command::ProfileImport {
                    specification: atelier::profile::VerificationProfile {
                        name: "井字棋可信检查".into(),
                        check_id: "tic-tac-toe-browser-v1".into(),
                        image: image.into(),
                        argv: vec![
                            "node".into(),
                            "/checks/check.mjs".into(),
                            "/candidate".into(),
                        ],
                    },
                },
            )
            .unwrap();
        let task = self.create();
        let id = task["task"]["id"].as_str().unwrap();
        self.store
            .execute(
                "contract",
                &Command::TaskUpdate {
                    decision_id: None,
                    id: id.into(),
                    revision: 1,
                    goal: None,
                    contract: ContractPatch {
                        code_input: Some(input["id"].as_str().unwrap().into()),
                        verification_profile: Some(profile["id"].as_str().unwrap().into()),
                        ..generic_contract()
                    },
                    refresh_team: false,
                },
            )
            .unwrap();
        self.store.runtime_register("service", 123).unwrap();
        let arranged = if let Some(config) = leader_config {
            let mailbox = self.store.mailbox(Some(&self.team.leader)).unwrap();
            let delivery = mailbox
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["message"]["kind"] == "intake.updated" && d["status"] == "queued")
                .unwrap()["id"]
                .as_str()
                .unwrap();
            let lead = self
                .store
                .runtime_claim("service", delivery, &config)
                .unwrap();
            self.store
                .runtime_begin_launch("service", &lead.id)
                .unwrap();
            let lead = self
                .store
                .runtime_child_started("service", &lead.id, 321, "fixture-leader")
                .unwrap();
            let bound = self.store.bind_member("service", &lead.id).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let accepted = runtime.block_on(async {
                let database = Database::open(self.dir.path().into(), 8).await.unwrap();
                database
                    .client()
                    .member_call(
                        bound.clone(),
                        member_operation(
                            &lead,
                            "accept",
                            "task_intake",
                            json!({"revision":2,"decision":"accept","reason":"固定输入完整"}),
                        ),
                    )
                    .await
                    .unwrap()
            });
            assert_eq!(accepted["ok"], true, "{accepted}");
            let result = self
                .store
                .member_call(
                    &bound,
                    &member_operation(
                        &lead,
                        "arrange",
                        "task_arrange",
                        json!({"revision":3,"action":"execute","instruction":"完成测试候选"}),
                    ),
                )
                .unwrap();
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(self.store.member_call(&bound,&member_operation(&lead,"finish","message_respond",json!({"kind":"assignment","messageId":result["data"]["delivery"]["messageId"]}))).unwrap()["ok"],true);
            self.store
                .runtime_run_observed_stopped("service", &lead.id, "fixture leader stopped")
                .unwrap();
            result["data"].clone()
        } else {
            self.store
                .execute(
                    "accept",
                    &Command::Intake {
                        id: id.into(),
                        revision: 2,
                        decision: IntakeDecision::Accept,
                        reason: "固定检查配置与输入齐备".into(),
                    },
                )
                .unwrap();
            self.store
                .execute(
                    "arrange",
                    &Command::TaskExecute {
                        id: id.into(),
                        revision: 3,
                        instruction: "完成测试候选".into(),
                    },
                )
                .unwrap()
        };
        let run = self
            .store
            .runtime_claim(
                "service",
                arranged["delivery"]["deliveryId"].as_str().unwrap(),
                &executor,
            )
            .unwrap();
        (run, verifier)
    }
    fn code_artifact_for_check(
        &mut self,
        html: &str,
        image: &str,
    ) -> (Run, atelier::member::MemberBinding) {
        let (run, verifier) = self.prepare_code_execution(image);
        let candidate = self
            .store
            .runtime_candidate_directory("service", &run.id)
            .unwrap();
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::write(candidate.join("index.html"), html).unwrap();
        self.store.runtime_begin_launch("service", &run.id).unwrap();
        let run = self
            .store
            .runtime_child_started("service", &run.id, 321, "fixture-producer")
            .unwrap();
        let binding = self.store.bind_member("service", &run.id).unwrap();
        assert_eq!(self.store.member_call(&binding,&member_operation(&run,"submit","artifact_submit",json!({"summary":"公开合成候选，非真实模型交付","handoff":"检查全部井字棋规则"}))).unwrap()["ok"],true);
        let artifact = self.fix_run(&run);
        self.claim_handoff(artifact.handoff_id.as_deref().unwrap(), &verifier)
    }
    async fn code_artifact_through_member_tools(
        &mut self,
        html: &str,
        image: &str,
    ) -> (Run, atelier::member::MemberBinding) {
        let (run, verifier) = self.prepare_code_execution(image);
        let database = Database::open(self.dir.path().into(), 8).await.unwrap();
        let client = database.client();
        client
            .runtime_prepare_candidate("service".into(), run.id.clone())
            .await
            .unwrap();
        self.store.runtime_begin_launch("service", &run.id).unwrap();
        let run = self
            .store
            .runtime_child_started("service", &run.id, 321, "fixture-member-files")
            .unwrap();
        let binding = self.store.bind_member("service", &run.id).unwrap();
        let written = client
            .member_call(
                binding.clone(),
                member_operation(
                    &run,
                    "write",
                    "write_file",
                    json!({"path":"index.html","content":html}),
                ),
            )
            .await
            .unwrap();
        assert_eq!(written["ok"], true, "{written}");
        assert_eq!(client.member_call(binding,member_operation(&run,"submit","artifact_submit",json!({"summary":"测试 fixture 经成员文件工具构造候选","handoff":"检查固定井字棋候选"}))).await.unwrap()["ok"],true);
        let artifact = client
            .runtime_fix_artifact(
                "service".into(),
                run.id,
                "测试：fixture 成员已停止且工具已处理完".into(),
            )
            .await
            .unwrap();
        database.close().await.unwrap();
        self.claim_handoff(artifact.handoff_id.as_deref().unwrap(), &verifier)
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires prepared immutable Docker image; run explicitly for integration evidence"]
async fn real_docker_check_collects_bound_evidence_and_cannot_be_overridden_by_model_advice() {
    let image = "sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133";
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".agents/verify-runs/1");
    std::fs::create_dir_all(&root).unwrap();
    let baseline = include_str!("../samples/tic-tac-toe/index.html");
    let original = "[[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8]]";
    let fixed = baseline.replace(
        original,
        "[[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8],[0,4,8],[2,4,6]]",
    );
    assert_ne!(fixed, baseline);
    let mut evidence = Vec::new();
    for (name, html, expected) in [
        ("known-defect", baseline, "fail"),
        ("test-only-reference", fixed.as_str(), "pass"),
    ] {
        let dir = tempfile::Builder::new()
            .prefix("core-check-")
            .tempdir_in(&root)
            .unwrap();
        let mut f = Fixture::in_directory(false, dir);
        let (run, binding) = f.code_artifact_through_member_tools(html, image).await;
        let check = member_operation(
            &run,
            "check",
            "run_check",
            json!({"checkId":"tic-tac-toe-browser-v1"}),
        );
        assert_eq!(
            f.store.member_call(&binding, &check).unwrap()["ok"],
            false,
            "接收前不能检查"
        );
        let h = f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["message"]["body"]
            .as_str()
            .unwrap()
            .to_string();
        let h: Value = serde_json::from_str(&h).unwrap();
        assert_eq!(f.store.member_call(&binding,&member_operation(&run,"accept","handoff_respond",json!({"id":h["handoffId"],"revision":1,"accept":true,"reason":"资料齐备，运行固定独立检查"}))).unwrap()["ok"],true);
        let database = Database::open(f.dir.path().into(), 8).await.unwrap();
        let operation = member_operation(&run, "accepted-check", "run_check", check.input.clone());
        let result = database
            .client()
            .member_call(binding.clone(), operation.clone())
            .await
            .unwrap();
        assert_eq!(result["ok"], true, "{result}");
        let c = &result["data"]["check"];
        assert_eq!(c["state"], "finished", "{c}");
        assert_eq!(c["conclusion"], expected, "{c}");
        assert_eq!(c["resources_stopped"], true);
        assert_eq!(c["isolation"]["network"], "none");
        assert_eq!(c["isolation"]["candidateReadonly"], true);
        assert_eq!(c["report"]["results"].as_array().unwrap().len(), 19);
        assert_eq!(
            database
                .client()
                .member_call(binding.clone(), operation)
                .await
                .unwrap(),
            result
        );
        let submitted=database.client().member_call(binding.clone(),member_operation(&run,"verification","verification_submit",json!({"evidenceId":c["id"],"recommendation":"pass","reason":"测试刻意给出 pass，失败证据必须仍为 fail"}))).await.unwrap();
        assert_eq!(submitted["ok"], true, "{submitted}");
        assert_eq!(submitted["data"]["conclusion"], expected);
        assert_eq!(
            f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
            "claimed"
        );
        f.store
            .runtime_run_observed_stopped(
                "service",
                &run.id,
                "测试：模型 fixture 已停止，真实检查容器已清理",
            )
            .unwrap();
        assert_eq!(
            f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
            "handled"
        );
        let persisted = f
            .store
            .verification(submitted["data"]["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(persisted.conclusion, expected);
        let stdout = f
            .store
            .check(c["id"].as_str().unwrap())
            .unwrap()
            .stdout
            .unwrap();
        let raw = std::fs::read(f.dir.path().join("objects/blobs").join(stdout.sha256)).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&raw).unwrap(), c["report"]);
        evidence.push(json!({"case":name,"check":c,"verification":submitted["data"]}));
        database.close().await.unwrap();
    }
    let path = root.join(format!("core-docker-check-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path,serde_json::to_vec_pretty(&json!({"productAcceptance":false,"realDockerChecks":true,"modelFixture":true,"evidence":evidence})).unwrap()).unwrap();
    println!("Docker integration evidence: {}", path.display());
}

#[test]
fn prepared_check_stops_without_launch_and_running_check_keeps_run_ownership() {
    for launched in [false, true] {
        let mut f = Fixture::new(false);
        let (run, binding) =
            f.code_artifact_for_check("<h1>test</h1>", &format!("sha256:{}", "a".repeat(64)));
        let message = f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["message"]["body"]
            .as_str()
            .unwrap()
            .to_string();
        let message: Value = serde_json::from_str(&message).unwrap();
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "accept",
                    "handoff_respond",
                    json!({"id":message["handoffId"],"revision":1,"accept":true,"reason":"接收"}),
                ),
            )
            .unwrap();
        let invalid = f
            .store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "wrong-check",
                    "run_check",
                    json!({"checkId":"model-selected-other-check"}),
                ),
            )
            .unwrap();
        assert_eq!(invalid["ok"], false);
        let result = f
            .store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "check",
                    "run_check",
                    json!({"checkId":"tic-tac-toe-browser-v1"}),
                ),
            )
            .unwrap();
        assert_eq!(result["ok"], true);
        let id = result["data"]["check"]["id"].as_str().unwrap();
        assert_eq!(
            f.store
                .member_call(
                    &binding,
                    &member_operation(
                        &run,
                        "fake-pass",
                        "verification_submit",
                        json!({"evidenceId":id,"recommendation":"pass","reason":"模型自报不算检查"})
                    )
                )
                .unwrap()["ok"],
            false
        );
        if launched {
            let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
            db.execute("UPDATE checks SET state='running',data=json_set(data,'$.state','running') WHERE id=?1",[id]).unwrap();
            assert!(matches!(
                f.store
                    .runtime_run_observed_stopped("service", &run.id, "不得跳过未核对检查"),
                Err(Error::Conflict(_))
            ));
            assert_eq!(f.store.run(&run.id).unwrap().state, "running");
        } else {
            f.store
                .runtime_run_observed_stopped("service", &run.id, "检查尚未启动即可停止")
                .unwrap();
            let c = f.store.check(id).unwrap();
            assert_eq!(c.state, "finished");
            assert_eq!(c.conclusion.as_deref(), Some("inconclusive"));
            assert!(c.resources_stopped);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires prepared immutable Docker image; run explicitly for recovery evidence"]
async fn real_docker_recovery_requires_ownership_and_keeps_other_run_resources_unknown() {
    let image = "sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133";
    let mut f = Fixture::new(false);
    let (run, binding) = f.code_artifact_for_check("<h1>fixture</h1>", image);
    let message: Value = serde_json::from_str(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["message"]["body"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(f.store.member_call(&binding, &member_operation(&run,"accept","handoff_respond",json!({"id":message["handoffId"],"revision":1,"accept":true,"reason":"恢复测试"}))).unwrap()["ok"],true);
    let result = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "check",
                "run_check",
                json!({"checkId":"tic-tac-toe-browser-v1"}),
            ),
        )
        .unwrap();
    let id = result["data"]["check"]["id"].as_str().unwrap();
    let name = result["data"]["check"]["container_name"].as_str().unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute(
        "UPDATE checks SET state='running',data=json_set(data,'$.state','running') WHERE id=?1",
        [id],
    )
    .unwrap();
    let budget = f.store.task(&run.task_id).unwrap().runs_used;
    f.store
        .runtime_register("recovery", std::process::id())
        .unwrap();
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    struct Container(String);
    impl Drop for Container {
        fn drop(&mut self) {
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", &self.0])
                .output();
        }
    }
    for owned in [false, true] {
        let created = std::process::Command::new("docker")
            .args([
                "run",
                "-d",
                "--pull=never",
                "--network",
                "none",
                "--read-only",
                "--user",
                "65534:65534",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--memory",
                "128m",
                "--pids-limit",
                "32",
                "--name",
                name,
                "--label",
                &format!("atelier.check={id}"),
                "--label",
                &format!(
                    "atelier.run={}",
                    if owned { &run.id } else { "foreign-run" }
                ),
                "--entrypoint",
                "node",
                image,
                "-e",
                "setInterval(()=>{},1000)",
            ])
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        let container = Container(name.into());
        let reconciled = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
        assert_eq!(reconciled["data"]["reconciledChecks"], usize::from(owned));
        assert_eq!(reconciled["data"]["state"], "blocked_unknown");
        let check = f.store.check(id).unwrap();
        assert_eq!(check.resources_stopped, owned);
        assert_eq!(check.state, if owned { "finished" } else { "unknown" });
        assert_eq!(check.conclusion.as_deref(), Some("inconclusive"));
        assert_eq!(
            f.store.run(&run.id).unwrap().state,
            "unknown",
            "清理检查容器不能代替其他执行资源核对"
        );
        assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, budget);
        let existing = std::process::Command::new("docker")
            .args(["inspect", name])
            .output()
            .unwrap();
        assert_eq!(existing.status.success(), !owned, "归属不符的容器必须保留");
        drop(container);
    }
    assert_eq!(
        acceptance_cli(&f, "unused", &["runtime", "reconcile"])["data"]["reconciledChecks"],
        0
    );
    assert!(
        database
            .client()
            .runtime_recover_checks("service".into())
            .await
            .is_err()
    );
    database.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn member_file_tools_publish_atomically_and_verifier_only_reads_fixed_artifact() {
    let mut f = Fixture::new(false);
    let (run, verifier) = f.prepare_code_execution(&format!("sha256:{}", "a".repeat(64)));
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    client
        .runtime_prepare_candidate("service".into(), run.id.clone())
        .await
        .unwrap();
    client
        .runtime_prepare_candidate("service".into(), run.id.clone())
        .await
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-file-tools")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    let initial = client
        .member_call(
            binding.clone(),
            member_operation(&run, "initial", "read_file", json!({"path":"index.html"})),
        )
        .await
        .unwrap();
    assert_eq!(initial["ok"], true, "{initial}");
    assert!(
        initial["data"]["content"]
            .as_str()
            .unwrap()
            .contains("[[0,1,2]")
    );
    let baseline_hash = initial["data"]["sha256"].as_str().unwrap().to_string();
    let write = member_operation(
        &run,
        "write",
        "write_file",
        json!({"path":"index.html","content":"<h1>成员工具写入的候选</h1>"}),
    );
    let written = client
        .member_call(binding.clone(), write.clone())
        .await
        .unwrap();
    assert_eq!(written["ok"], true, "{written}");
    assert_eq!(written["data"]["revision"], 2);
    assert_eq!(
        client.member_call(binding.clone(), write).await.unwrap(),
        written
    );
    let fixed_input =
        std::fs::read_to_string(f.dir.path().join("objects/blobs").join(baseline_hash)).unwrap();
    assert!(fixed_input.contains("[[0,1,2]"), "不能改写固定输入");
    let old_snapshot = f
        .store
        .runtime_prepare_artifact("service", &run.id)
        .unwrap()
        .fix()
        .unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_file_ledger BEFORE INSERT ON member_requests WHEN NEW.operation_id='op-rollback-write' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let rollback = member_operation(
        &run,
        "rollback-write",
        "write_file",
        json!({"path":"new/file.txt","content":"先落盘但不能提前发布"}),
    );
    assert!(
        client
            .member_call(binding.clone(), rollback.clone())
            .await
            .is_err()
    );
    let listed = client
        .member_call(
            binding.clone(),
            member_operation(
                &run,
                "list-before-retry",
                "list_files",
                json!({"prefix":"new"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(listed["data"]["total"], 0);
    assert_eq!(listed["data"]["revision"], 2);
    sql.execute_batch("DROP TRIGGER fail_file_ledger;").unwrap();
    let retried = client.member_call(binding.clone(), rollback).await.unwrap();
    assert_eq!(retried["ok"], true);
    assert_eq!(retried["data"]["revision"], 3);
    assert!(matches!(
        f.store
            .runtime_publish_artifact("service", old_snapshot, "旧快照不能覆盖更新后的候选"),
        Err(Error::Conflict(_))
    ));
    assert_eq!(f.store.run(&run.id).unwrap().state, "running");
    let deleted = client
        .member_call(
            binding.clone(),
            member_operation(
                &run,
                "delete",
                "delete_file",
                json!({"path":"new/file.txt"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(deleted["ok"], true);
    assert_eq!(deleted["data"]["revision"], 4);
    let submit = client
        .member_call(
            binding.clone(),
            member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"测试构造成员调用，非模型交付","handoff":"检查固定候选"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(submit["ok"], true);
    let rejected = client
        .member_call(
            binding.clone(),
            member_operation(
                &run,
                "write-after-submit",
                "write_file",
                json!({"path":"index.html","content":"非法修改"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(rejected["ok"], false);
    let artifact = client
        .runtime_fix_artifact(
            "service".into(),
            run.id.clone(),
            "测试：fixture 成员已停止且工具已处理完".into(),
        )
        .await
        .unwrap();
    assert!(!artifact.partial);
    let file = artifact
        .files
        .iter()
        .find(|f| f.path == "index.html")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(f.dir.path().join("objects/blobs").join(&file.sha256)).unwrap(),
        "<h1>成员工具写入的候选</h1>"
    );
    assert!(!artifact.files.iter().any(|f| f.path == "new/file.txt"));
    let (verify, bound) = f.claim_handoff(artifact.handoff_id.as_deref().unwrap(), &verifier);
    let read = client
        .member_call(
            bound.clone(),
            member_operation(&verify, "read", "read_file", json!({"path":"index.html"})),
        )
        .await
        .unwrap();
    assert_eq!(read["data"]["viewId"], artifact.id);
    assert_eq!(read["data"]["content"], "<h1>成员工具写入的候选</h1>");
    for (i, name) in ["write_file", "delete_file"].iter().enumerate() {
        let input = if *name == "write_file" {
            json!({"path":"index.html","content":"篡改"})
        } else {
            json!({"path":"index.html"})
        };
        let denied = client
            .member_call(
                bound.clone(),
                member_operation(&verify, &format!("deny-{i}"), name, input),
            )
            .await
            .unwrap();
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["error"]["code"], "forbidden");
    }
    let description = f.store.member_description(&bound).unwrap();
    assert!(
        !description["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "write_file")
    );
    database.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn file_tools_reject_escaping_paths_and_links_and_bound_utf8_results() {
    let mut f = Fixture::new(false);
    let (run, _) = f.prepare_code_execution(&format!("sha256:{}", "a".repeat(64)));
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    client
        .runtime_prepare_candidate("service".into(), run.id.clone())
        .await
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-file-boundary")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    for (i, path) in [
        "../atelier.sqlite3",
        "/etc/passwd",
        "nested/../../outside",
        ".git/config",
        "x\\..\\secret",
        "index.html/child",
    ]
    .iter()
    .enumerate()
    {
        let result = client
            .member_call(
                binding.clone(),
                member_operation(
                    &run,
                    &format!("bad-{i}"),
                    "write_file",
                    json!({"path":path,"content":"forbidden"}),
                ),
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], false, "{result}");
    }
    let content = "中文\n\"".repeat(12000);
    assert_eq!(
        client
            .member_call(
                binding.clone(),
                member_operation(
                    &run,
                    "unicode-write",
                    "write_file",
                    json!({"path":"unicode.txt","content":content})
                )
            )
            .await
            .unwrap()["ok"],
        true
    );
    let mut joined = String::new();
    let mut offset = 0;
    for n in 0..10 {
        let read = client
            .member_call(
                binding.clone(),
                member_operation(
                    &run,
                    &format!("chunk-{n}"),
                    "read_file",
                    json!({"path":"unicode.txt","offset":offset}),
                ),
            )
            .await
            .unwrap();
        assert_eq!(read["ok"], true, "{read}");
        assert!(serde_json::to_vec(&read).unwrap().len() <= 256 * 1024);
        joined.push_str(read["data"]["content"].as_str().unwrap());
        if let Some(next) = read["data"]["nextOffset"].as_u64() {
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(joined, content);
    let readop = member_operation(
        &run,
        "original-read",
        "read_file",
        json!({"path":"index.html"}),
    );
    let original = client
        .member_call(binding.clone(), readop.clone())
        .await
        .unwrap();
    let hash = original["data"]["sha256"].as_str().unwrap();
    let blob = f.dir.path().join("objects/blobs").join(hash);
    let bytes = std::fs::read(&blob).unwrap();
    std::fs::remove_file(&blob).unwrap();
    let private = f.dir.path().join("private-test-file");
    std::fs::write(&private, "private fixture must not leak").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&private, &blob).unwrap();
    let damaged = client
        .member_call(
            binding.clone(),
            member_operation(&run, "damaged", "read_file", json!({"path":"index.html"})),
        )
        .await
        .unwrap();
    assert_eq!(damaged["ok"], false);
    assert!(
        !damaged
            .to_string()
            .contains("private fixture must not leak")
    );
    std::fs::remove_file(&blob).unwrap();
    std::fs::write(&blob, bytes).unwrap();
    let mut revoke = BTreeMap::new();
    revoke.insert(run.worker_id.clone(), vec![Permission::Execute]);
    f.store
        .execute(
            "revoke-file-access",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: f.store.team(&f.team.id).unwrap().authorization_revision,
                grant: BTreeMap::new(),
                revoke,
            },
        )
        .unwrap();
    assert!(
        client.member_call(binding, readop).await.is_err(),
        "撤权后不能读取缓存"
    );
    database.close().await.unwrap();
}

#[test]
fn credential_input_validation_does_not_create_secret_storage_or_business_requests() {
    let mut f = Fixture::new(false);
    for invalid in ["", " leading", "trailing ", "line\nbreak"] {
        assert!(matches!(
            f.store.set_credential("invalid", "missing", 1, invalid),
            Err(Error::Invalid(_))
        ));
    }
    assert!(matches!(
        f.store.execute(
            "ordinary",
            &Command::CredentialSet {
                id: "missing".into(),
                revision: 1,
                version: None,
            }
        ),
        Err(Error::Invalid(_))
    ));
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM credential_requests", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "writes synthetic credentials to the real macOS Keychain and removes those test entries"]
fn real_keychain_credentials_are_versioned_private_and_retryable() {
    let mut f = Fixture::new(false);
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(db) = rusqlite::Connection::open(&self.0) {
                if let Ok(mut query) = db.prepare(
                    "SELECT json_extract(data,'$.reference.account') FROM credential_requests",
                ) {
                    if let Ok(rows) = query.query_map([], |r| r.get::<_, String>(0)) {
                        for account in rows.flatten() {
                            let deleted = security_framework::passwords::delete_generic_password(
                                "io.xforce.atelier.api",
                                &account,
                            );
                            if !std::thread::panicking() {
                                assert!(
                                    deleted.is_ok() || deleted.is_err_and(|e| e.code() == -25300),
                                    "测试 Keychain 项必须清理"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let _cleanup = Cleanup(f.dir.path().join("atelier.sqlite3"));
    let created = f
        .store
        .execute(
            "connection-keychain",
            &Command::ConnectionCreate {
                name: "真实 Keychain 合成测试".into(),
                specification: atelier::connection::ConnectionSpec::Api {
                    protocol: atelier::connection::ApiProtocol::OpenaiChatCompletions,
                    model: "test-model".into(),
                    base_url: Some("https://example.invalid".into()),
                },
            },
        )
        .unwrap();
    let id = created["connection"]["id"].as_str().unwrap();
    let version = created["version"]["id"].as_str().unwrap();
    let secret = format!("synthetic-keychain-test-{}", uuid::Uuid::new_v4());
    let result = json!({"data":f.store.set_credential("credential-one",id,1,&secret).unwrap()});
    assert_eq!(result["data"]["credentialGeneration"], 1);
    let reference = f.store.credential_reference(version).unwrap().unwrap();
    assert_eq!(
        atelier::credential::load_secret(&reference).unwrap(),
        secret
    );
    assert_eq!(
        f.store
            .set_credential("credential-one", id, 1, &secret)
            .unwrap(),
        result["data"]
    );
    assert!(matches!(
        f.store
            .set_credential("credential-one", id, 1, "different-synthetic-secret"),
        Err(Error::Conflict(_))
    ));
    let rotated = f
        .store
        .set_credential("credential-two", id, 1, "second-synthetic-keychain-secret")
        .unwrap();
    assert_eq!(rotated["credentialGeneration"], 2);
    assert_eq!(
        f.store
            .set_credential("credential-one", id, 1, &secret)
            .unwrap()["credentialGeneration"],
        1
    );
    assert_eq!(
        f.store.connection_view(id).unwrap()["credentialGeneration"],
        2
    );
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_credential_publication BEFORE INSERT ON requests WHEN NEW.id='credential-retry' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(matches!(
        f.store
            .set_credential("credential-retry", id, 1, "third-synthetic-keychain-secret"),
        Err(Error::Database(_))
    ));
    assert_eq!(
        f.store.connection_view(id).unwrap()["credentialGeneration"],
        2
    );
    db.execute_batch("DROP TRIGGER fail_credential_publication;")
        .unwrap();
    assert_eq!(
        f.store
            .set_credential("credential-retry", id, 1, "third-synthetic-keychain-secret")
            .unwrap()["credentialGeneration"],
        3
    );
    assert!(matches!(
        f.store.execute(
            "credential-retry",
            &Command::ConnectionCreate {
                name: "不能复用请求".into(),
                specification: atelier::connection::ConnectionSpec::AgentCli {
                    runtime: "pi".into(),
                    model: None,
                    image: None,
                    egress_hosts: None,
                    egress_proxy: None,
                }
            }
        ),
        Err(Error::Conflict(_))
    ));
    f.store
        .execute(
            "connection-version",
            &Command::ConnectionUpdate {
                id: id.into(),
                revision: 1,
                name: None,
                specification: Some(atelier::connection::ConnectionSpec::Api {
                    protocol: atelier::connection::ApiProtocol::OpenaiChatCompletions,
                    model: "new-model".into(),
                    base_url: Some("https://other.example.invalid".into()),
                }),
            },
        )
        .unwrap();
    assert!(
        f.store.connection_view(id).unwrap()["credentialGeneration"].is_null(),
        "新连接版本不能自动继承旧凭据"
    );
    assert_eq!(
        atelier::credential::load_secret(&reference).unwrap(),
        secret,
        "旧冻结版本的引用仍可用"
    );
    let repaired = f
        .store
        .set_credential_version(
            "repair-frozen-version",
            id,
            2,
            Some(version),
            "repaired-synthetic-old-version-secret",
        )
        .unwrap();
    assert_eq!(repaired["credentialGeneration"], 4);
    assert!(f.store.connection_view(id).unwrap()["credentialGeneration"].is_null());
    let old_current = f.store.credential_reference(version).unwrap().unwrap();
    assert_eq!(
        atelier::credential::load_secret(&old_current).unwrap(),
        "repaired-synthetic-old-version-secret"
    );
    db.execute_batch("CREATE TRIGGER fail_clear_publication BEFORE INSERT ON requests WHEN NEW.id='clear-retry' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(matches!(
        f.store
            .clear_credential("clear-retry", id, 2, Some(version)),
        Err(Error::Database(_))
    ));
    assert!(atelier::credential::load_secret(&old_current).is_err());
    db.execute_batch("DROP TRIGGER fail_clear_publication;")
        .unwrap();
    let cleared = f
        .store
        .clear_credential("clear-retry", id, 2, Some(version))
        .unwrap();
    assert!(f.store.credential_reference(version).unwrap().is_none());
    let new = f
        .store
        .set_credential_version(
            "after-clear",
            id,
            2,
            Some(version),
            "after-clear-synthetic-secret",
        )
        .unwrap();
    assert_eq!(new["credentialGeneration"], 5);
    assert_eq!(
        f.store
            .clear_credential("clear-retry", id, 2, Some(version))
            .unwrap(),
        cleared
    );
    assert_eq!(
        f.store
            .credential_reference(version)
            .unwrap()
            .unwrap()
            .generation,
        5
    );
    for entry in std::fs::read_dir(f.dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(path).unwrap();
            for plaintext in [
                &secret,
                "second-synthetic-keychain-secret",
                "third-synthetic-keychain-secret",
                "repaired-synthetic-old-version-secret",
                "after-clear-synthetic-secret",
            ] {
                assert!(
                    !bytes
                        .windows(plaintext.len())
                        .any(|w| w == plaintext.as_bytes()),
                    "工作区不得保存凭据明文"
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "uses the real CLI and macOS Keychain with synthetic test credentials"]
fn real_keychain_cli_accepts_only_stdin_and_replays_without_secret_output() {
    use std::io::Write;
    let mut f = Fixture::new(false);
    let created = f
        .store
        .execute(
            "connection-cli-secret",
            &Command::ConnectionCreate {
                name: "CLI 合成凭据测试".into(),
                specification: atelier::connection::ConnectionSpec::Api {
                    protocol: atelier::connection::ApiProtocol::OpenaiChatCompletions,
                    model: "fixture".into(),
                    base_url: Some("https://example.invalid".into()),
                },
            },
        )
        .unwrap();
    let id = created["connection"]["id"].as_str().unwrap();
    let version = created["version"]["id"].as_str().unwrap();
    let invoke = |request: &str, secret: &str| {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(f.dir.path())
            .args([
                "--json",
                "--request-id",
                request,
                "connection",
                "credential",
                "set",
                id,
                "--revision",
                "1",
                "--version",
                version,
                "--stdin",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(secret.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output.status.success(), value)
    };
    let secret = format!("synthetic-cli-only-secret-{}", uuid::Uuid::new_v4());
    let invalid = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(f.dir.path())
        .args([
            "--json",
            "connection",
            "credential",
            "set",
            id,
            "--revision",
            "1",
            "--secret",
            &secret,
        ])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stdout).contains(&secret));
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains(&secret));
    let first = invoke("cli-secret", &secret);
    assert!(first.0, "{}", first.1);
    struct Cleanup {
        workspace: std::path::PathBuf,
        id: String,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
                .arg("--workspace")
                .arg(&self.workspace)
                .args([
                    "--json",
                    "--request-id",
                    "cli-final-clear",
                    "connection",
                    "credential",
                    "clear",
                    &self.id,
                    "--revision",
                    "1",
                ])
                .output()
                .unwrap();
            if !std::thread::panicking() {
                assert!(
                    output.status.success(),
                    "CLI 必须清理自己的合成 Keychain 项"
                );
            }
        }
    }
    let _cleanup = Cleanup {
        workspace: f.dir.path().into(),
        id: id.into(),
    };
    let replay = invoke("cli-secret", &secret);
    assert_eq!(
        replay, first,
        "同一 CLI 新进程能够读取 Keychain 并核对原请求"
    );
    let diagnostic = || {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(f.dir.path())
            .args([
                "--json",
                "--request-id",
                "cli-connection-test",
                "connection",
                "test",
                id,
                "--revision",
                "1",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&secret));
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
    };
    let begun = std::time::Instant::now();
    let probe = diagnostic();
    assert!(begun.elapsed() < Duration::from_secs(32));
    assert!(
        matches!(
            probe["code"].as_str(),
            Some("connection_failed" | "deadline")
        ),
        "必须走实际 API 探测，不把接入错误算作提供商错误：{probe}"
    );
    assert_eq!(probe["credentialGeneration"], 1);
    assert_eq!(diagnostic(), probe, "同请求不再次发出模型请求");
    assert!(f.store.list("task").unwrap().as_array().unwrap().is_empty());
    let mismatch = invoke("cli-secret", "different-synthetic-cli-secret");
    assert!(!mismatch.0);
    assert_eq!(mismatch.1["error"]["code"], "conflict");
    assert_eq!(f.store.request("cli-secret").unwrap(), first.1["data"]);
    let clear = |request: &str| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(f.dir.path())
            .args([
                "--json",
                "--request-id",
                request,
                "connection",
                "credential",
                "clear",
                id,
                "--revision",
                "1",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let cleared = clear("cli-clear");
    assert!(f.store.credential_reference(version).unwrap().is_none());
    assert_eq!(
        f.store.connection_view(id).unwrap()["readiness"],
        "unchecked",
        "清除凭据使旧诊断失效"
    );
    let next = invoke("cli-second-secret", "replacement-synthetic-cli-secret");
    assert!(next.0);
    assert_eq!(
        next.1["data"]["credentialGeneration"], 2,
        "删除后代次不能归零"
    );
    assert_eq!(diagnostic(), probe, "新凭据不改变原请求结果或隐式发起重测");
    assert_eq!(
        f.store.connection_view(id).unwrap()["readiness"],
        "unchecked"
    );
    assert_eq!(clear("cli-clear"), cleared);
    assert_eq!(
        f.store
            .credential_reference(version)
            .unwrap()
            .unwrap()
            .generation,
        2,
        "旧清除操作不能删除新代次"
    );
    let bytes = std::fs::read(f.dir.path().join("atelier.sqlite3")).unwrap();
    assert!(!bytes.windows(secret.len()).any(|b| b == secret.as_bytes()));
}

#[test]
fn native_context_binding_survives_restart_and_separates_task_purpose_and_configuration() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let task = created["task"]["id"].as_str().unwrap();
    let worker = f.team.leader.clone();
    f.store.runtime_register("context-service", 123).unwrap();
    let context = f
        .store
        .runtime_context("context-service", task, &worker, &config, "coordinate")
        .unwrap();
    context.prepare_directories(f.dir.path()).unwrap();
    assert!(!context.used);
    let reopened = Store::open(f.dir.path())
        .unwrap()
        .runtime_context("context-service", task, &worker, &config, "coordinate")
        .unwrap();
    assert_eq!(context.id, reopened.id);
    let execute = f
        .store
        .runtime_context("context-service", task, &worker, &config, "execute")
        .unwrap();
    let rework = f
        .store
        .runtime_context("context-service", task, &worker, &config, "rework")
        .unwrap();
    let verify = f
        .store
        .runtime_context("context-service", task, &worker, &config, "verify")
        .unwrap();
    assert_eq!(execute.id, rework.id);
    assert_ne!(context.id, execute.id);
    assert_ne!(execute.id, verify.id);
    let other = f
        .store
        .execute(
            "other-task",
            &Command::TaskCreate {
                team_id: f.team.id.clone(),
                goal: "另一个任务".into(),
            },
        )
        .unwrap();
    let other = f
        .store
        .runtime_context(
            "context-service",
            other["task"]["id"].as_str().unwrap(),
            &worker,
            &config,
            "coordinate",
        )
        .unwrap();
    assert_ne!(context.id, other.id);
    f.store
        .execute(
            "goal-change",
            &Command::TaskUpdate {
                decision_id: None,
                id: task.into(),
                revision: 1,
                goal: Some("补充同一任务的目标".into()),
                contract: Default::default(),
                refresh_team: false,
            },
        )
        .unwrap();
    let revised = f
        .store
        .runtime_context("context-service", task, &worker, &config, "coordinate")
        .unwrap();
    assert_eq!(context.id, revised.id, "普通契约修改不丢弃原生上下文");
    f.store
        .execute(
            "role-description",
            &Command::WorkerUpdate {
                id: worker.clone(),
                revision: 2,
                name: None,
                description: Some("新的长期职责".into()),
                connection: None,
                clear_connection: false,
            },
        )
        .unwrap();
    f.store
        .execute(
            "refresh-context",
            &Command::TaskUpdate {
                decision_id: None,
                id: task.into(),
                revision: 2,
                goal: None,
                contract: Default::default(),
                refresh_team: true,
            },
        )
        .unwrap();
    let refreshed = f
        .store
        .runtime_context("context-service", task, &worker, &config, "coordinate")
        .unwrap();
    assert_ne!(context.id, refreshed.id, "冻结职责变化必须隔离旧协调上下文");
    assert!(
        f.store
            .runtime_context(
                "context-service",
                task,
                &worker,
                "other-config",
                "coordinate"
            )
            .is_err()
    );
    f.store
        .runtime_register("context-service-two", 456)
        .unwrap();
    assert_eq!(
        refreshed.id,
        f.store
            .runtime_context("context-service-two", task, &worker, &config, "coordinate")
            .unwrap()
            .id
    );
    assert!(
        f.store
            .runtime_context("context-service", task, &worker, &config, "coordinate")
            .is_err()
    );
}

#[test]
fn api_launch_atomically_marks_context_used_and_never_recreates_missing_history() {
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let task = created["task"]["id"].as_str().unwrap();
    let worker = f.team.leader.clone();
    f.store.runtime_register("api-intent", 123).unwrap();
    let context = f
        .store
        .runtime_context("api-intent", task, &worker, &config, "coordinate")
        .unwrap();
    context.prepare_directories(f.dir.path()).unwrap();
    let run = f
        .store
        .runtime_claim(
            "api-intent",
            created["delivery"]["deliveryId"].as_str().unwrap(),
            &config,
        )
        .unwrap();
    let version = f
        .store
        .execution_configuration(&config)
        .unwrap()
        .connection_version;
    // A metadata-only fixture exercises the transaction, never OS credentials or a model.
    let reference = atelier::credential::CredentialReference {
        connection_version: version.clone(),
        generation: 1,
        account: "context-test-metadata-only".into(),
    };
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO credentials(version_id,data) VALUES(?1,?2)",
        rusqlite::params![version, serde_json::to_string(&reference).unwrap()],
    )
    .unwrap();
    assert!(
        f.store
            .runtime_begin_api_launch("api-intent", &run.id, &context, 2)
            .is_err()
    );
    assert!(!f.store.run(&run.id).unwrap().launch_started);
    assert!(
        !f.store
            .runtime_context("api-intent", task, &worker, &config, "coordinate")
            .unwrap()
            .used
    );
    db.execute_batch("CREATE TRIGGER reject_launch BEFORE INSERT ON api_launches BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.store
            .runtime_begin_api_launch("api-intent", &run.id, &context, 1)
            .is_err()
    );
    assert!(
        !f.store
            .runtime_context("api-intent", task, &worker, &config, "coordinate")
            .unwrap()
            .used
    );
    assert!(!f.store.run(&run.id).unwrap().launch_started);
    db.execute_batch("DROP TRIGGER reject_launch;").unwrap();
    f.store
        .runtime_begin_api_launch("api-intent", &run.id, &context, 1)
        .unwrap();
    let used = f
        .store
        .runtime_context("api-intent", task, &worker, &config, "coordinate")
        .unwrap();
    assert!(used.used);
    assert_eq!(used.id, context.id);
    assert!(
        used.prepare_directories(f.dir.path()).is_err(),
        "启动可能发生后，不能把缺失 checkpoint 当作空上下文"
    );
    assert_eq!(
        f.store.run_view(&run.id).unwrap()["apiExecution"]["contextId"],
        context.id
    );
    assert!(
        f.store
            .runtime_begin_api_launch("api-intent", &run.id, &context, 1)
            .is_err()
    );
    f.store.runtime_register("api-restarted", 456).unwrap();
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    assert!(
        f.store
            .runtime_context("api-restarted", task, &worker, &config, "coordinate")
            .unwrap()
            .used
    );
    let (native, _) = used.directories(f.dir.path());
    std::fs::remove_dir(&native).unwrap();
    assert!(used.prepare_directories(f.dir.path()).is_err());
    assert!(!native.exists(), "禁止静默创建丢失的原生会话目录");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "production service, Node adapter and real Keychain; synthetic credential, invalid provider endpoint"]
fn real_keychain_runtime_owns_api_child_and_stop_does_not_handle_unfinished_delivery() {
    use std::io::Write;
    let mut f = Fixture::new(true);
    let configuration = f.prepare_digital_leader();
    let connection_version = f
        .store
        .execution_configuration(&configuration)
        .unwrap()
        .connection_version;
    let connection = f
        .store
        .connection_version(&connection_version)
        .unwrap()
        .connection_id;
    let invoke = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(f.dir.path())
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
    };
    let mut setting = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(f.dir.path())
        .args([
            "--json",
            "--request-id",
            "runtime-secret",
            "connection",
            "credential",
            "set",
            &connection,
            "--revision",
            "1",
            "--stdin",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let secret = format!("synthetic-runtime-test-{}", uuid::Uuid::new_v4());
    setting
        .stdin
        .take()
        .unwrap()
        .write_all(secret.as_bytes())
        .unwrap();
    let output = setting.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&secret));
    struct Cleanup {
        path: std::path::PathBuf,
        connection: String,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let base = || {
                let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"));
                c.arg("--workspace").arg(&self.path).arg("--json");
                c
            };
            let _ = base().args(["runtime", "stop"]).output();
            let clear = base()
                .args([
                    "--request-id",
                    "runtime-clear",
                    "connection",
                    "credential",
                    "clear",
                    &self.connection,
                    "--revision",
                    "1",
                ])
                .output()
                .unwrap();
            if !std::thread::panicking() {
                assert!(clear.status.success(), "必须删除合成 Keychain 项");
            }
        }
    }
    let _cleanup = Cleanup {
        path: f.dir.path().into(),
        connection: connection.clone(),
    };
    let created = f
        .store
        .execute(
            "runtime-task",
            &Command::TaskCreate {
                team_id: f.team.id.clone(),
                goal: "合成任务：仅测试独立执行进程的启动和停止，不声称业务完成".into(),
            },
        )
        .unwrap();
    let task = created["task"]["id"].as_str().unwrap();
    assert_eq!(invoke(&["runtime", "start"])["state"], "running");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let run = loop {
        let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
        if let Some(id) = mailbox[0]["runId"].as_str() {
            let run = f.store.run(id).unwrap();
            if run.pid.is_some() {
                break run;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "生产服务须实际启动 Node 接入；mailbox={mailbox}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    invoke(&["runtime", "stop"]);
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    loop {
        let state = invoke(&["runtime", "status"]);
        if state["state"] == "stopped" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "服务须实际回收自己的子进程：{state}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let view = invoke(&["run", "show", &run.id]);
    assert_eq!(view["state"], "stopped");
    assert_eq!(view["apiExecution"]["adapterStopped"], true);
    assert_eq!(view["apiExecution"]["credentialGeneration"], 1);
    assert!(!view.to_string().contains(&secret));
    assert_eq!(f.store.task(task).unwrap().runs_used, 1);
    assert_eq!(f.store.task(task).unwrap().state, "pending");
    let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
    assert_eq!(
        mailbox.as_array().unwrap().len(),
        1,
        "失败不会制造新的负责人唤起循环"
    );
    assert_eq!(mailbox[0]["status"], "blocked");
    assert!(mailbox[0]["handlingResult"].is_null());
    assert_eq!(f.store.mailbox(None).unwrap()[0]["status"], "queued");
    let db = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    let used: bool = db
        .query_row(
            "SELECT json_extract(data,'$.used') FROM execution_contexts",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(used);
    let bytes = std::fs::read(f.dir.path().join("atelier.sqlite3")).unwrap();
    assert!(!bytes.windows(secret.len()).any(|b| b == secret.as_bytes()));
    assert_eq!(invoke(&["runtime", "start"])["activeRuns"], 0);
    invoke(&["runtime", "stop"]);
    assert_eq!(
        f.store.task(task).unwrap().runs_used,
        1,
        "重启不会重派已阻塞的投递"
    );
    let crash_task = f
        .store
        .execute(
            "crash-task",
            &Command::TaskCreate {
                team_id: f.team.id.clone(),
                goal: "合成任务：服务崩溃后的资源核对".into(),
            },
        )
        .unwrap();
    let delivery = crash_task["delivery"]["deliveryId"].as_str().unwrap();
    let service = invoke(&["runtime", "start"]);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let crash_run = loop {
        let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
        let receipt = mailbox
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == delivery)
            .unwrap();
        if let Some(id) = receipt["runId"].as_str() {
            let run = f.store.run(id).unwrap();
            if run.pid.is_some() {
                break run;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "崩溃场景须启动真实 API 子进程"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(crash_run.state, "running");
    let killed = std::process::Command::new("/bin/kill")
        .args(["-KILL", &service["pid"].as_u64().unwrap().to_string()])
        .status()
        .unwrap();
    assert!(killed.success(), "只终止本测试刚启动的服务进程");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while invoke(&["runtime", "status"])["lockHeld"] == true {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    loop {
        let reconciled = invoke(&["runtime", "reconcile"]);
        assert_ne!(reconciled["epoch"], service["epoch"]);
        if reconciled["state"] == "stopped" {
            break;
        }
        assert_eq!(reconciled["state"], "blocked_unknown");
        assert!(
            std::time::Instant::now() < deadline,
            "旧接入应因断管退出，显式核对其进程组后才能释放 Run"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let recovered = invoke(&["run", "show", &crash_run.id]);
    assert_eq!(recovered["apiExecution"]["recovered"], true);
    assert_eq!(recovered["apiExecution"]["adapterStopped"], true);
    assert_eq!(f.store.task(&crash_run.task_id).unwrap().runs_used, 1);
    let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
    let receipt = mailbox
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == delivery)
        .unwrap();
    assert_eq!(receipt["status"], "blocked");
    assert!(receipt["handlingResult"].is_null());
    assert_eq!(recovery_for(&f, &crash_run.task_id).state, "open");
    assert_eq!(invoke(&["runtime", "start"])["activeRuns"], 0);
    invoke(&["runtime", "stop"]);
}

#[test]
fn runtime_cancels_stale_ordinary_message_without_run_or_failure_notification() {
    let mut f = Fixture::new(true);
    let created = f.create();
    let task = created["task"]["id"].as_str().unwrap();
    let note = f
        .store
        .execute(
            "old-note",
            &Command::MessageSend {
                task_id: task.into(),
                recipient: f.team.leader.clone(),
                kind: "work.note".into(),
                body: "旧任务依据下的说明".into(),
                reply_to: None,
            },
        )
        .unwrap();
    f.store
        .execute(
            "new-goal",
            &Command::TaskUpdate {
                decision_id: None,
                id: task.into(),
                revision: 1,
                goal: Some("更新的目标".into()),
                contract: Default::default(),
                refresh_team: false,
            },
        )
        .unwrap();
    let invoke = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(f.dir.path())
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    };
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
                .arg("--workspace")
                .arg(&self.0)
                .args(["runtime", "stop"])
                .output();
        }
    }
    let _cleanup = Cleanup(f.dir.path().into());
    invoke(&["runtime", "start"]);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
        if mailbox
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["status"] != "queued")
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    invoke(&["runtime", "stop"]);
    let mailbox = f.store.mailbox(Some(&f.team.leader)).unwrap();
    let old = mailbox
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == note["deliveryId"])
        .unwrap();
    assert_eq!(old["status"], "cancelled");
    assert_eq!(old["attempts"], 0);
    assert_eq!(f.store.task(task).unwrap().runs_used, 0);
    let human = f.store.mailbox(None).unwrap();
    assert_eq!(
        human.as_array().unwrap().len(),
        1,
        "只通知新承接的缺配置问题，不给旧版本说明制造失败待办"
    );
    let body: Value = serde_json::from_str(human[0]["message"]["body"].as_str().unwrap()).unwrap();
    assert_ne!(body["deliveryId"], note["deliveryId"]);
}

#[tokio::test(flavor = "current_thread")]
async fn self_test_snapshot_and_member_ledger_commit_together_without_delivery_effects() {
    let mut f = Fixture::new(false);
    let (run, _) = f.prepare_code_execution(&format!("sha256:{}", "a".repeat(64)));
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    client
        .runtime_prepare_candidate("service".into(), run.id.clone())
        .await
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-self-test")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    let description = f.store.member_description(&binding).unwrap();
    let names: Vec<_> = description["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"run_check"));
    assert!(!names.contains(&"verification_submit"));
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_self_test_ledger BEFORE INSERT ON member_requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let operation = member_operation(
        &run,
        "self-check",
        "run_check",
        json!({"checkId":"tic-tac-toe-browser-v1"}),
    );
    assert!(f.store.member_call(&binding, &operation).is_err());
    for table in ["checks", "check_inputs", "member_requests"] {
        let n: i64 = sql
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table} must roll back");
    }
    sql.execute_batch("DROP TRIGGER fail_self_test_ledger;")
        .unwrap();
    let queued = f.store.member_call(&binding, &operation).unwrap();
    assert_eq!(queued["ok"], true, "{queued}");
    assert_eq!(queued["data"]["check"]["target"]["kind"], "candidate");
    let id = queued["data"]["check"]["id"].as_str().unwrap();
    let snapshot: String = sql
        .query_row("SELECT data FROM check_inputs WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    let written = client
        .member_call(
            binding.clone(),
            member_operation(
                &run,
                "change-after-request",
                "write_file",
                json!({"path":"index.html","content":"changed after check request"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(written["ok"], true, "{written}");
    assert_eq!(f.store.member_call(&binding, &operation).unwrap(), queued);
    let persisted: String = sql
        .query_row("SELECT data FROM check_inputs WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(snapshot, persisted);
    let candidate: String = sql
        .query_row(
            "SELECT data FROM candidates WHERE run_id=?1",
            [&run.id],
            |r| r.get(0),
        )
        .unwrap();
    let candidate: Value = serde_json::from_str(&candidate).unwrap();
    assert!(
        candidate["revision"].as_u64().unwrap()
            > queued["data"]["check"]["target"]["revision"]
                .as_u64()
                .unwrap()
    );
    let duplicate = f
        .store
        .member_call(
            &binding,
            &member_operation(&run, "second-active", "run_check", operation.input.clone()),
        )
        .unwrap();
    assert_eq!(duplicate["ok"], false);
    let forged = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "forge-verification",
                "verification_submit",
                json!({"evidenceId":id,"recommendation":"pass","reason":"attempt self approval"}),
            ),
        )
        .unwrap();
    assert_eq!(forged["ok"], false);
    assert_eq!(forged["error"]["code"], "forbidden");
    for table in ["artifacts", "handoffs", "verifications"] {
        let n: i64 = sql
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
    assert_eq!(f.store.task(&run.task_id).unwrap().state, "active");
    f.store
        .runtime_run_observed_stopped(
            "service",
            &run.id,
            "fixture stopped before launching prepared check",
        )
        .unwrap();
    assert_eq!(
        f.store.check(id).unwrap().conclusion.as_deref(),
        Some("inconclusive")
    );
    database.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires prepared immutable Docker image; run explicitly for integration evidence"]
async fn real_docker_self_test_uses_requested_snapshot_and_pass_never_becomes_verification() {
    let image = "sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133";
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".agents/verify-runs/1");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("self-test-")
        .tempdir_in(&root)
        .unwrap();
    let mut f = Fixture::in_directory(false, dir);
    let (run, _) = f.prepare_code_execution(image);
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    client
        .runtime_prepare_candidate("service".into(), run.id.clone())
        .await
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-self-test")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    let operation = member_operation(
        &run,
        "old-snapshot",
        "run_check",
        json!({"checkId":"tic-tac-toe-browser-v1"}),
    );
    let requested = f.store.member_call(&binding, &operation).unwrap();
    assert_eq!(requested["ok"], true, "{requested}");
    let baseline = include_str!("../samples/tic-tac-toe/index.html");
    let fixed = baseline.replace(
        "[[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8]]",
        "[[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8],[0,4,8],[2,4,6]]",
    );
    assert_ne!(baseline, fixed);
    let written = client
        .member_call(
            binding.clone(),
            member_operation(
                &run,
                "repair",
                "write_file",
                json!({"path":"index.html","content":fixed}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(written["ok"], true, "{written}");
    // Restart the database owner before launch: the old snapshot must survive.
    database.close().await.unwrap();
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    let old = client
        .member_call(binding.clone(), operation.clone())
        .await
        .unwrap();
    assert_eq!(old["ok"], true, "{old}");
    assert_eq!(old["data"]["check"]["conclusion"], "fail", "{old}");
    assert_eq!(
        old["data"]["check"]["content_digest"],
        requested["data"]["check"]["content_digest"]
    );
    let repaired = client
        .member_call(
            binding.clone(),
            member_operation(&run, "new-snapshot", "run_check", operation.input.clone()),
        )
        .await
        .unwrap();
    assert_eq!(repaired["ok"], true, "{repaired}");
    let check = &repaired["data"]["check"];
    assert_eq!(check["conclusion"], "pass", "{check}");
    assert_eq!(check["target"]["kind"], "candidate");
    assert_eq!(check["isolation"]["network"], "none");
    assert_eq!(check["isolation"]["candidateReadonly"], true);
    assert_eq!(check["resources_stopped"], true);
    assert!(
        check["target"]["revision"].as_u64().unwrap()
            > old["data"]["check"]["target"]["revision"].as_u64().unwrap()
    );
    assert_ne!(
        check["content_digest"],
        old["data"]["check"]["content_digest"]
    );
    let denied = client.member_call(binding.clone(), member_operation(&run, "forge-pass", "verification_submit", json!({"evidenceId":check["id"],"recommendation":"pass","reason":"self test passed"}))).await.unwrap();
    assert_eq!(denied["ok"], false);
    let submitted = client
        .member_call(
            binding.clone(),
            member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"test-only reference fix"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(submitted["ok"], true);
    let late = client
        .member_call(
            binding.clone(),
            member_operation(&run, "after-submit", "run_check", operation.input.clone()),
        )
        .await
        .unwrap();
    assert_eq!(late["ok"], false);
    assert_eq!(
        client.member_call(binding, operation).await.unwrap(),
        old,
        "replay remains readable after submission"
    );
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    let n: i64 = sql
        .query_row("SELECT count(*) FROM verifications", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    let n: i64 = sql
        .query_row("SELECT count(*) FROM handoffs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(f.store.task(&run.task_id).unwrap().state, "active");
    let artifact = client
        .runtime_fix_artifact(
            "service".into(),
            run.id.clone(),
            "fixture member stopped and checks reaped".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        artifact.content_digest,
        check["content_digest"].as_str().unwrap()
    );
    std::fs::write(root.join(format!("self-test-{}.json", uuid::Uuid::new_v4())), serde_json::to_vec_pretty(&json!({"scope":"core integration with fixture member; not Story acceptance","oldSnapshot":old,"newSnapshot":repaired,"verificationDenied":denied,"artifactId":artifact.id})).unwrap()).unwrap();
    database.close().await.unwrap();
}

#[test]
fn rework_reservation_is_atomic_persistent_and_consumed_once_with_bounded_retries() {
    let mut f = Fixture::new(false);
    let (first, _, _) = f.running_executor();
    let reason = atelier::rework::ReworkReason::RunFailure {
        id: first.id.clone(),
    };
    let command = Command::TaskRework {
        id: first.task_id.clone(),
        revision: first.task_revision,
        reason,
        instruction: "在保留内容上继续，不重放旧调用".into(),
    };
    assert!(f.store.execute("while-running", &command).is_err());
    let artifact = f.fix_run(&first);
    assert!(artifact.partial);
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    let before = f.store.task(&first.task_id).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_rework BEFORE INSERT ON requests WHEN NEW.id='rework-1' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.store.execute("rework-1", &command).is_err());
    assert_eq!(
        f.store.task_details(&first.task_id).unwrap()["reworks_reserved"],
        0
    );
    assert_eq!(
        f.store.task(&first.task_id).unwrap().messages_used,
        before.messages_used
    );
    sql.execute_batch("DROP TRIGGER fail_rework;").unwrap();
    let arranged = f.store.execute("rework-1", &command).unwrap();
    assert_eq!(arranged["rework"]["baseArtifact"], artifact.id);
    assert_eq!(arranged["budget"]["reworksUsed"], 0);
    assert_eq!(arranged["budget"]["reworksReserved"], 1);
    assert_eq!(f.store.execute("rework-1", &command).unwrap(), arranged);
    assert!(f.store.execute("duplicate", &command).is_err());
    f.store = Store::open(f.dir.path()).unwrap();
    f.store.runtime_register("restarted", 123).unwrap();
    assert_eq!(
        f.store.task_details(&first.task_id).unwrap()["reworks_reserved"],
        1
    );
    let delivery = arranged["delivery"]["deliveryId"].as_str().unwrap();
    sql.execute_batch("CREATE TRIGGER fail_rework_claim BEFORE UPDATE ON deliveries WHEN NEW.status='claimed' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.store
            .runtime_claim("restarted", delivery, &first.configuration_id)
            .is_err()
    );
    assert_eq!(f.store.task(&first.task_id).unwrap().reworks_used, 0);
    assert_eq!(
        f.store.task_details(&first.task_id).unwrap()["reworks_reserved"],
        1
    );
    sql.execute_batch("DROP TRIGGER fail_rework_claim;")
        .unwrap();
    let second = f
        .store
        .runtime_claim("restarted", delivery, &first.configuration_id)
        .unwrap();
    assert_eq!(second.purpose, "rework");
    assert_eq!(f.store.task(&first.task_id).unwrap().reworks_used, 1);
    assert_eq!(
        f.store.task(&first.task_id).unwrap().runs_used,
        before.runs_used + 1
    );
    assert_eq!(
        f.store.task_details(&first.task_id).unwrap()["reworks_reserved"],
        0
    );
    assert!(
        f.store
            .runtime_claim("restarted", delivery, &first.configuration_id)
            .is_err()
    );
    f.store
        .runtime_run_observed_stopped(
            "restarted",
            &second.id,
            "fixture preparation failed before launch",
        )
        .unwrap();
    assert!(f.store.execute("stale-reason", &command).is_err());
    let next = Command::TaskRework {
        id: first.task_id.clone(),
        revision: first.task_revision,
        reason: atelier::rework::ReworkReason::RunFailure {
            id: second.id.clone(),
        },
        instruction: "处理本次准备错误".into(),
    };
    let arranged = f.store.execute("rework-2", &next).unwrap();
    let third = f
        .store
        .runtime_claim(
            "restarted",
            arranged["delivery"]["deliveryId"].as_str().unwrap(),
            &first.configuration_id,
        )
        .unwrap();
    f.store
        .runtime_run_observed_stopped("restarted", &third.id, "fixture stopped")
        .unwrap();
    f.store.runtime_register("again", 123).unwrap();
    let denied = f.store.execute(
        "over-budget",
        &Command::TaskRework {
            id: first.task_id.clone(),
            revision: first.task_revision,
            reason: atelier::rework::ReworkReason::RunFailure { id: third.id },
            instruction: "额度不得重置".into(),
        },
    );
    assert!(matches!(denied, Err(Error::Conflict(_))));
    assert_eq!(f.store.task(&first.task_id).unwrap().reworks_used, 2);
    assert_eq!(
        f.store.task_details(&first.task_id).unwrap()["rework_arrangements"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn cancelling_unclaimed_rework_releases_reservation_and_success_is_not_failure_reason() {
    let mut f = Fixture::new(false);
    let (run, _, _) = f.running_executor();
    f.fix_run(&run);
    f.store
        .execute(
            "revoke-executor",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: f.store.team(&f.team.id).unwrap().revision,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(run.worker_id.clone(), vec![Permission::Execute])]),
            },
        )
        .unwrap();

    let command = Command::TaskRework {
        id: run.task_id.clone(),
        revision: run.task_revision,
        reason: atelier::rework::ReworkReason::RunFailure { id: run.id },
        instruction: "继续".into(),
    };
    let arranged = f.store.execute("rework", &command).unwrap();
    assert_eq!(arranged["delivery"]["status"], "blocked");
    assert!(
        f.store
            .runtime_claim(
                "service",
                arranged["delivery"]["deliveryId"].as_str().unwrap(),
                &run.configuration_id
            )
            .is_err()
    );

    assert_eq!(
        f.store.task_details(&run.task_id).unwrap()["reworks_reserved"],
        1
    );
    f.store
        .execute(
            "cancel",
            &Command::TaskCancel {
                id: run.task_id.clone(),
                revision: run.task_revision,
                reason: "本人取消排队工作".into(),
            },
        )
        .unwrap();
    let details = f.store.task_details(&run.task_id).unwrap();
    assert_eq!(details["reworks_reserved"], 0);
    assert_eq!(details["reworks_used"], 0);
    assert_eq!(details["rework_arrangements"][0]["budgetState"], "released");
    let mut f = Fixture::new(false);
    let (run, bound, _) = f.running_executor();
    assert_eq!(
        f.store
            .member_call(
                &bound,
                &member_operation(&run, "submit", "artifact_submit", json!({"summary":"完成"}))
            )
            .unwrap()["ok"],
        true
    );
    f.fix_run(&run);
    assert!(
        f.store
            .execute(
                "cannot-invent-failure",
                &Command::TaskRework {
                    id: run.task_id,
                    revision: run.task_revision,
                    reason: atelier::rework::ReworkReason::RunFailure { id: run.id },
                    instruction: "没有失败依据的重做".into()
                }
            )
            .is_err()
    );
}

#[test]
fn digital_leader_can_arrange_rework_from_result_and_human_cannot_impersonate_it() {
    let mut f = Fixture::new(true);
    let executor = f.prepare_executor();
    let (lead, bound) = f.running_member_with_contract(Some(generic_contract()));
    assert_eq!(
        f.store
            .member_call(
                &bound,
                &member_operation(
                    &lead,
                    "accept",
                    "task_intake",
                    json!({"revision":2,"decision":"accept","reason":"资料完整"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let first = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &lead,
                "arrange",
                "task_arrange",
                json!({"revision":3,"action":"execute","instruction":"执行"}),
            ),
        )
        .unwrap();
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(
        f.store
            .member_call(
                &bound,
                &member_operation(
                    &lead,
                    "respond",
                    "message_respond",
                    json!({"kind":"assignment","messageId":first["data"]["delivery"]["messageId"]})
                )
            )
            .unwrap()["ok"],
        true
    );
    f.store
        .runtime_run_observed_stopped("service", &lead.id, "fixture leader stopped")
        .unwrap();
    let exec = f
        .store
        .runtime_claim(
            "service",
            first["data"]["delivery"]["deliveryId"].as_str().unwrap(),
            &executor,
        )
        .unwrap();
    f.store
        .runtime_run_observed_stopped("service", &exec.id, "fixture failed before launch")
        .unwrap();
    // Explicitly deliver a task-scoped note as a fixture stimulus; it is not a rework arrangement.
    let notice = f
        .store
        .execute(
            "notify",
            &Command::MessageSend {
                task_id: lead.task_id.clone(),
                recipient: lead.worker_id.clone(),
                kind: "work.note".into(),
                body: "请查看已核对的执行失败".into(),
                reply_to: None,
            },
        )
        .unwrap();
    let next = f
        .store
        .runtime_claim(
            "service",
            notice["deliveryId"].as_str().unwrap(),
            &lead.configuration_id,
        )
        .unwrap();
    f.store.runtime_begin_launch("service", &next.id).unwrap();
    let next = f
        .store
        .runtime_child_started("service", &next.id, 321, "fixture-leader-rework")
        .unwrap();
    let bound = f.store.bind_member("service", &next.id).unwrap();
    let reason = atelier::rework::ReworkReason::RunFailure { id: exec.id };
    assert!(matches!(
        f.store.execute(
            "human-cannot-arrange",
            &Command::TaskRework {
                id: next.task_id.clone(),
                revision: 3,
                reason: reason.clone(),
                instruction: "代办".into()
            }
        ),
        Err(Error::Forbidden(_))
    ));
    let operation = member_operation(
        &next,
        "rework",
        "task_arrange",
        json!({"revision":3,"action":"rework","reason":reason,"instruction":"在原契约范围内重新执行"}),
    );
    let arranged = f.store.member_call(&bound, &operation).unwrap();
    assert_eq!(arranged["ok"], true, "{arranged}");
    assert_eq!(f.store.member_call(&bound, &operation).unwrap(), arranged);
    assert_eq!(f.store.member_call(&bound,&member_operation(&next,"finish","message_respond",json!({"kind":"assignment","messageId":arranged["data"]["delivery"]["messageId"]}))).unwrap()["ok"],true);
    f.store
        .runtime_run_observed_stopped("service", &next.id, "fixture leader stopped")
        .unwrap();
    let rework = f
        .store
        .runtime_claim(
            "service",
            arranged["data"]["delivery"]["deliveryId"].as_str().unwrap(),
            &executor,
        )
        .unwrap();
    assert_eq!(rework.purpose, "rework");
    assert_eq!(f.store.task(&next.task_id).unwrap().reworks_used, 1);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires prepared immutable Docker image; fixture members, real CLI and checks"]
async fn real_docker_failed_verification_cli_rework_and_new_independent_verification() {
    real_docker_rework_acceptance_scenario(false).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires prepared immutable Docker image; verifies CLI human rejection and acceptance"]
async fn real_docker_human_rejection_cli_rework_and_new_acceptance() {
    real_docker_rework_acceptance_scenario(true).await;
}

async fn real_docker_rework_acceptance_scenario(human_rejected: bool) {
    let image = "sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133";
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".agents/verify-runs/1");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("rework-")
        .tempdir_in(&root)
        .unwrap();
    let mut f = Fixture::in_directory(false, dir);
    let baseline = include_str!("../samples/tic-tac-toe/index.html");
    let initial = if human_rejected {
        repaired_tic_tac_toe()
    } else {
        baseline.into()
    };
    let (verify, bound) = f.code_artifact_through_member_tools(&initial, image).await;
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    let task = f.store.task(&verify.task_id).unwrap();
    let old_artifact = f
        .store
        .artifact(task.current_artifact.as_deref().unwrap())
        .unwrap();
    let h = old_artifact.handoff_id.as_deref().unwrap();
    assert_eq!(
        f.store
            .member_call(
                &bound,
                &member_operation(
                    &verify,
                    "accept",
                    "handoff_respond",
                    json!({"id":h,"revision":1,"accept":true,"reason":"资料齐全"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let check = client
        .member_call(
            bound.clone(),
            member_operation(
                &verify,
                "check",
                "run_check",
                json!({"checkId":"tic-tac-toe-browser-v1"}),
            ),
        )
        .await
        .unwrap();
    let initial_conclusion = if human_rejected { "pass" } else { "fail" };
    assert_eq!(
        check["data"]["check"]["conclusion"], initial_conclusion,
        "{check}"
    );
    let failed=client.member_call(bound.clone(),member_operation(&verify,"verify","verification_submit",json!({"evidenceId":check["data"]["check"]["id"],"recommendation":initial_conclusion,"reason":"实际独立检查的结果"}))).await.unwrap();
    assert_eq!(failed["ok"], true, "{failed}");
    assert_eq!(
        f.store.task_details(&task.id).unwrap()["rework_arrangements"],
        json!([]),
        "检验失败不能自行安排返工"
    );
    let command = Command::TaskRework {
        id: task.id.clone(),
        revision: task.revision,
        reason: atelier::rework::ReworkReason::Verification {
            id: failed["data"]["id"].as_str().unwrap().into(),
        },
        instruction: "修复对角线并保留其它规则".into(),
    };
    assert!(
        f.store
            .execute("before-verifier-stopped", &command)
            .is_err()
    );
    f.store
        .runtime_run_observed_stopped(
            "service",
            &verify.id,
            "fixture verifier stopped after real failed check",
        )
        .unwrap();
    let old_request = human_rejected.then(|| {
        request_acceptance_cli(
            &f,
            &task,
            &old_artifact.id,
            failed["data"]["id"].as_str().unwrap(),
            "first-acceptance",
        )
    });
    let rejection = old_request
        .as_ref()
        .map(|r| decide_acceptance_cli(&f, &task, r, false, "human-rejection"));
    if human_rejected {
        assert_eq!(
            rejection.as_ref().unwrap()["data"]["task"]["state"],
            "active"
        );
        let v: atelier::verification::Verification =
            serde_json::from_value(failed["data"].clone()).unwrap();
        assert!(
            f.store
                .execute("retry-rejected-artifact", &acceptance_request_command(&v))
                .is_err()
        );
    }
    let reason_id = if let Some(r) = &old_request {
        r["data"]["decision"]["id"].as_str().unwrap()
    } else {
        failed["data"]["id"].as_str().unwrap()
    };
    let command = if human_rejected {
        Command::TaskRework {
            id: task.id.clone(),
            revision: task.revision,
            reason: atelier::rework::ReworkReason::Rejection {
                id: reason_id.into(),
            },
            instruction: "根据人类拒绝补充新版".into(),
        }
    } else {
        command
    };
    let invoke = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(f.dir.path())
            .args([
                "--json",
                "--request-id",
                "cli-rework",
                "task",
                "rework",
                &task.id,
                "--revision",
                &task.revision.to_string(),
                if human_rejected {
                    "--rejection"
                } else {
                    "--verification"
                },
                reason_id,
                "--instruction",
                "修复对角线并保留其它规则",
            ])
            .output()
            .unwrap()
    };
    let output = invoke();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let arranged: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&invoke().stdout).unwrap(),
        arranged
    );
    assert_eq!(arranged["data"]["budget"]["reworksReserved"], 1);
    assert_eq!(arranged["data"]["rework"]["baseArtifact"], old_artifact.id);
    assert!(f.store.execute("duplicate-rework", &command).is_err());
    assert!(
        f.store
            .execute(
                "old-artifact-handoff",
                &Command::TaskVerify {
                    verification_id: None,
                    blocker_id: None,
                    id: task.id.clone(),
                    revision: task.revision,
                    artifact_id: old_artifact.id.clone(),
                    instruction: "不能与返工并存".into()
                }
            )
            .is_err()
    );
    let executor = task.team_snapshot.executor.as_deref().unwrap();
    let config = task.worker_snapshots[executor]
        .execution_config
        .as_deref()
        .unwrap();
    let context = f
        .store
        .runtime_context("service", &task.id, executor, config, "execute")
        .unwrap();
    let resumed = f
        .store
        .runtime_context("service", &task.id, executor, config, "rework")
        .unwrap();
    assert_eq!(
        context.id, resumed.id,
        "用途族绑定复用；fixture 不声称已验证真实模型原生续接"
    );
    let run = f
        .store
        .runtime_claim(
            "service",
            arranged["data"]["delivery"]["deliveryId"].as_str().unwrap(),
            config,
        )
        .unwrap();
    client
        .runtime_prepare_candidate("service".into(), run.id.clone())
        .await
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-rework-files")
        .unwrap();
    let bound = f.store.bind_member("service", &run.id).unwrap();
    let before = client
        .member_call(
            bound.clone(),
            member_operation(&run, "read", "read_file", json!({"path":"index.html"})),
        )
        .await
        .unwrap();
    assert_eq!(before["ok"], true, "{before}");
    // Snapshot equality is also checked through the database manifest to cover all files.
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    let data: String = sql
        .query_row(
            "SELECT data FROM candidates WHERE run_id=?1",
            [&run.id],
            |r| r.get(0),
        )
        .unwrap();
    let candidate: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(
        candidate["files"],
        serde_json::to_value(&old_artifact.files).unwrap()
    );
    let fixed = if human_rejected {
        format!(
            "{}\n<!-- 补充交付说明：离线双人游戏，包含双方对角线判胜。 -->\n",
            repaired_tic_tac_toe()
        )
    } else {
        repaired_tic_tac_toe()
    };
    let written = client
        .member_call(
            bound.clone(),
            member_operation(
                &run,
                "repair",
                "write_file",
                json!({"path":"index.html","content":fixed}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(written["ok"], true, "{written}");
    let submitted = client
        .member_call(
            bound,
            member_operation(
                &run,
                "submit",
                "artifact_submit",
                json!({"summary":"测试参考修复两条对角线","handoff":"请独立重验全部规则"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(submitted["ok"], true, "{submitted}");
    let artifact = client
        .runtime_fix_artifact(
            "service".into(),
            run.id.clone(),
            "fixture executor stopped; no check resources active".into(),
        )
        .await
        .unwrap();
    assert_ne!(artifact.id, old_artifact.id);
    assert_ne!(artifact.content_digest, old_artifact.content_digest);
    assert!(f.store.execute("stale-verification", &command).is_err());
    let verifier_config = task.worker_snapshots[task.team_snapshot.verifier.as_deref().unwrap()]
        .execution_config
        .as_deref()
        .unwrap();
    let (verify, bound) = f.claim_handoff(artifact.handoff_id.as_deref().unwrap(), verifier_config);
    assert_eq!(f.store.member_call(&bound,&member_operation(&verify,"accept","handoff_respond",json!({"id":artifact.handoff_id,"revision":1,"accept":true,"reason":"新版产出已收到"}))).unwrap()["ok"],true);
    let stale=client.member_call(bound.clone(),member_operation(&verify,"old-check","verification_submit",json!({"evidenceId":check["data"]["check"]["id"],"recommendation":"pass","reason":"旧证据不能替代新版检验"}))).await.unwrap();
    assert_eq!(stale["ok"], false);
    let new_check = client
        .member_call(
            bound.clone(),
            member_operation(
                &verify,
                "check",
                "run_check",
                json!({"checkId":"tic-tac-toe-browser-v1"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        new_check["data"]["check"]["conclusion"], "pass",
        "{new_check}"
    );
    let passed=client.member_call(bound,member_operation(&verify,"verify","verification_submit",json!({"evidenceId":new_check["data"]["check"]["id"],"recommendation":"pass","reason":"新版全部规则通过"}))).await.unwrap();
    assert_eq!(passed["data"]["conclusion"], "pass", "{passed}");
    f.store
        .runtime_run_observed_stopped(
            "service",
            &verify.id,
            "fixture verifier stopped after independent check",
        )
        .unwrap();
    assert_eq!(
        f.store
            .verification(failed["data"]["id"].as_str().unwrap())
            .unwrap()
            .conclusion,
        initial_conclusion
    );
    assert_eq!(f.store.task(&task.id).unwrap().reworks_used, 1);
    assert_eq!(f.store.task(&task.id).unwrap().state, "active");
    let details_output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(f.dir.path())
        .args(["--json", "task", "show", &task.id])
        .output()
        .unwrap();
    assert!(details_output.status.success());
    let details: Value = serde_json::from_slice(&details_output.stdout).unwrap();
    assert_eq!(details["data"]["reworks_reserved"], 0);
    assert_eq!(
        details["data"]["rework_arrangements"][0]["budgetState"],
        "consumed"
    );
    let exports = tempfile::tempdir().unwrap();
    for (name, version, html) in [
        ("historical", &old_artifact, initial.as_str()),
        ("current", &artifact, fixed.as_str()),
    ] {
        let target = exports.path().join(name);
        let exported = acceptance_cli(
            &f,
            &format!("export-{name}"),
            &[
                "artifact",
                "export",
                &version.id,
                "--destination",
                target.to_str().unwrap(),
            ],
        );
        assert_eq!(exported["data"]["contentDigest"], version.content_digest);
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).unwrap(),
            html
        );
        assert_eq!(f.store.task(&task.id).unwrap().state, "active");
    }
    let request = request_acceptance_cli(
        &f,
        &task,
        &artifact.id,
        passed["data"]["id"].as_str().unwrap(),
        "new-acceptance",
    );
    let accepted = decide_acceptance_cli(&f, &task, &request, true, "human-acceptance");
    assert_eq!(accepted["data"]["task"]["state"], "closed");
    assert_eq!(accepted["data"]["task"]["outcome"], "accepted");
    if let Some(old) = &old_request {
        assert_eq!(
            f.store
                .decision(old["data"]["decision"]["id"].as_str().unwrap())
                .unwrap()
                .state,
            "rejected"
        );
    }
    std::fs::write(root.join(format!("rework-{}.json",uuid::Uuid::new_v4())),serde_json::to_vec_pretty(&json!({"productAcceptance":false,"modelFixture":true,"realCli":true,"realDocker":true,"failed":failed,"arranged":arranged,"newCheck":new_check,"passed":passed,"details":details,"rejection":rejection,"request":request,"accepted":accepted})).unwrap()).unwrap();
    database.close().await.unwrap();
}

#[test]
fn reported_blocker_is_atomic_requires_observed_stop_and_cannot_be_bypassed_as_run_failure() {
    let mut f = Fixture::new(false);
    let (run, bound, _) = f.running_executor();
    let rejected = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &run,
                "invalid-handler",
                "task_report_blocker",
                json!({"handler":f.team.verifier,"reason":"未授权成员不能被指定为修复协调者"}),
            ),
        )
        .unwrap();
    assert_eq!(rejected["ok"], false);
    assert!(!f.store.run(&run.id).unwrap().stop_requested);
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    let op = member_operation(
        &run,
        "blocker",
        "task_report_blocker",
        json!({"handler":f.human,"reason":"缺少原契约内的环境条件"}),
    );
    sql.execute_batch("CREATE TRIGGER fail_blocker_ledger BEFORE INSERT ON member_requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.store.member_call(&bound, &op).is_err());
    assert!(!f.store.run(&run.id).unwrap().stop_requested);
    assert_eq!(f.store.blockers(&run.task_id).unwrap(), json!([]));
    sql.execute_batch("DROP TRIGGER fail_blocker_ledger;")
        .unwrap();
    let reported = f.store.member_call(&bound, &op).unwrap();
    assert_eq!(reported["ok"], true, "{reported}");
    let id = reported["data"]["blocker"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(reported["data"]["resourcesStopped"], false);
    assert!(f.store.run(&run.id).unwrap().stop_requested);
    assert_eq!(f.store.run(&run.id).unwrap().state, "running");
    assert!(
        f.store.member_call(&bound, &op).is_err(),
        "停止请求后旧成员通道不能继续提交；记录可由管理查询读取"
    );
    assert_eq!(
        f.store
            .blockers(&run.task_id)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let resolve = Command::BlockerResolve {
        id: id.clone(),
        revision: 1,
        task_revision: run.task_revision,
        evidence: "补齐原契约所需环境，未改变输入或标准".into(),
    };
    assert!(f.store.execute("early-resolve", &resolve).is_err());
    let artifact = f.fix_run(&run);
    assert!(artifact.partial);
    assert_eq!(
        f.store.mailbox(Some(&run.worker_id)).unwrap()[0]["status"],
        "handled"
    );
    let rework = Command::TaskRework {
        id: run.task_id.clone(),
        revision: run.task_revision,
        reason: atelier::rework::ReworkReason::Blocker { id: id.clone() },
        instruction: "继续原工作".into(),
    };
    assert!(f.store.execute("unresolved-rework", &rework).is_err());
    assert!(
        f.store
            .execute(
                "fake-failure",
                &Command::TaskRework {
                    id: run.task_id.clone(),
                    revision: run.task_revision,
                    reason: atelier::rework::ReworkReason::RunFailure { id: run.id.clone() },
                    instruction: "不能绕过未解决阻塞".into()
                }
            )
            .is_err()
    );
    sql.execute_batch("CREATE TRIGGER fail_resolution BEFORE INSERT ON messages WHEN NEW.kind='resolved' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.store.execute("resolve", &resolve).is_err());
    assert_eq!(f.store.blocker(&id).unwrap().state, "open");
    sql.execute_batch("DROP TRIGGER fail_resolution;").unwrap();
    let saved = f.store.execute("resolve", &resolve).unwrap();
    assert_eq!(saved["state"], "resolved");
    assert_eq!(f.store.execute("resolve", &resolve).unwrap(), saved);
    assert_eq!(
        f.store.task_details(&run.task_id).unwrap()["rework_arrangements"],
        json!([]),
        "解决不自动安排继续"
    );
    let arranged = f.store.execute("rework", &rework).unwrap();
    assert_eq!(arranged["rework"]["baseArtifact"], artifact.id);
    assert_eq!(arranged["budget"]["reworksReserved"], 1);
}

#[tokio::test(flavor = "current_thread")]
async fn empty_blocked_execution_stops_without_artifact_then_cli_resolves_and_arranges_rework() {
    let mut f = Fixture::new(false);
    let (run, _) = f.prepare_code_execution(&format!("sha256:{}", "a".repeat(64)));
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    client
        .runtime_prepare_candidate("service".into(), run.id.clone())
        .await
        .unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-empty-blocker")
        .unwrap();
    let bound = f.store.bind_member("service", &run.id).unwrap();
    let files = client
        .member_call(
            bound.clone(),
            member_operation(&run, "files", "list_files", json!({})),
        )
        .await
        .unwrap();
    let files = files["data"]["files"].as_array().unwrap();
    assert!(!files.is_empty());
    for (i, file) in files.iter().enumerate() {
        let deleted = client
            .member_call(
                bound.clone(),
                member_operation(
                    &run,
                    &format!("delete-{i}"),
                    "delete_file",
                    json!({"path":file["path"]}),
                ),
            )
            .await
            .unwrap();
        assert_eq!(deleted["ok"], true, "{deleted}");
    }
    let reported = client
        .member_call(
            bound,
            member_operation(
                &run,
                "report",
                "task_report_blocker",
                json!({"handler":f.human,"reason":"原范围内的执行条件缺失，当前没有可交付文件"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(reported["ok"], true, "{reported}");
    let id = reported["data"]["blocker"]["id"].as_str().unwrap();
    f.store.runtime_register("recovery", 456).unwrap();
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    assert!(
        f.store
            .execute(
                "unknown-resolve",
                &Command::BlockerResolve {
                    id: id.into(),
                    revision: 1,
                    task_revision: run.task_revision,
                    evidence: "尚未核对资源，不能提前解决".into()
                }
            )
            .is_err()
    );
    let finished = client
        .runtime_finish_execution(
            "recovery".into(),
            run.id.clone(),
            "fixture resource owner confirmed no processes or pending writes".into(),
        )
        .await
        .unwrap();
    assert!(finished.is_none());
    assert!(f.store.artifact_for_run(&run.id).unwrap().is_none());
    assert_eq!(f.store.run(&run.id).unwrap().state, "stopped");
    let invoke = |request: &str, args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(f.dir.path())
            .args(["--json", "--request-id", request])
            .args(args)
            .output()
            .unwrap()
    };
    let rev = run.task_revision.to_string();
    let args = [
        "task",
        "blocker",
        "resolve",
        id,
        "--revision",
        "1",
        "--task-revision",
        &rev,
        "--evidence",
        "环境已在原契约范围内修复",
    ];
    let output = invoke("cli-resolve", &args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let resolved: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&invoke("cli-resolve", &args).stdout).unwrap(),
        resolved
    );
    assert_eq!(
        f.store.task_details(&run.task_id).unwrap()["rework_arrangements"],
        json!([])
    );
    let output = invoke(
        "cli-rework",
        &[
            "task",
            "rework",
            &run.task_id,
            "--revision",
            &rev,
            "--blocker",
            id,
            "--instruction",
            "在原输入和预算内继续",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let arranged: Value = serde_json::from_slice(&output.stdout).unwrap();
    let next = f
        .store
        .runtime_claim(
            "recovery",
            arranged["data"]["delivery"]["deliveryId"].as_str().unwrap(),
            &run.configuration_id,
        )
        .unwrap();
    client
        .runtime_prepare_candidate("recovery".into(), next.id.clone())
        .await
        .unwrap();
    assert_eq!(next.purpose, "rework");
    assert_eq!(f.store.task(&run.task_id).unwrap().reworks_used, 1);
    database.close().await.unwrap();
}

#[test]
fn resolved_verification_blocker_requires_explicit_new_handoff_and_preserves_old_history() {
    for accepted in [false, true] {
        let mut f = Fixture::new(false);
        let (run, bound) =
            f.code_artifact_for_check("<h1>fixture</h1>", &format!("sha256:{}", "a".repeat(64)));
        let task = f.store.task(&run.task_id).unwrap();
        let artifact = f
            .store
            .artifact(task.current_artifact.as_deref().unwrap())
            .unwrap();
        let old = artifact.handoff_id.as_deref().unwrap();
        if accepted {
            assert_eq!(f.store.member_call(&bound,&member_operation(&run,"accept","handoff_respond",json!({"id":old,"revision":1,"accept":true,"reason":"资料完整但检查环境尚有问题"}))).unwrap()["ok"],true);
        }
        let reported = f
            .store
            .member_call(
                &bound,
                &member_operation(
                    &run,
                    "blocker",
                    "task_report_blocker",
                    json!({"handler":f.human,"reason":"可信检查环境不可用"}),
                ),
            )
            .unwrap();
        assert_eq!(reported["ok"], true, "{reported}");
        let id = reported["data"]["blocker"]["id"].as_str().unwrap();
        f.store
            .runtime_run_observed_stopped("service", &run.id, "fixture verifier resources stopped")
            .unwrap();
        f.store
            .execute(
                "resolve",
                &Command::BlockerResolve {
                    id: id.into(),
                    revision: 1,
                    task_revision: task.revision,
                    evidence: "原冻结检查环境已准备，未换检查镜像或规则".into(),
                },
            )
            .unwrap();
        let command = Command::TaskVerify {
            verification_id: None,
            blocker_id: Some(id.into()),
            id: task.id.clone(),
            revision: task.revision,
            artifact_id: artifact.id.clone(),
            instruction: "重新接收并独立检查".into(),
        };
        let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
        sql.execute_batch("CREATE TRIGGER fail_new_handoff BEFORE INSERT ON messages WHEN NEW.kind='handoff.verify' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(f.store.execute("reverify", &command).is_err());
        assert_eq!(
            f.store.handoff(old).unwrap().state,
            if accepted { "accepted" } else { "offered" }
        );
        sql.execute_batch("DROP TRIGGER fail_new_handoff;").unwrap();
        let offered = f.store.execute("reverify", &command).unwrap();
        assert_eq!(f.store.execute("reverify", &command).unwrap(), offered);
        assert_eq!(f.store.handoff(old).unwrap().state, "superseded");
        let historical = f.store.handoff(old).unwrap();
        assert_eq!(
            historical.prior_state.as_deref(),
            Some(if accepted { "accepted" } else { "offered" })
        );
        assert_eq!(historical.superseded_by_blocker.as_deref(), Some(id));
        assert_eq!(
            historical.reason.as_deref(),
            if accepted {
                Some("资料完整但检查环境尚有问题")
            } else {
                None
            }
        );

        assert_ne!(offered["handoff"]["id"], old);
        assert!(f.store.execute("reuse-old-blocker", &command).is_err());
        let next = f
            .store
            .runtime_claim(
                "service",
                offered["delivery"]["deliveryId"].as_str().unwrap(),
                &run.configuration_id,
            )
            .unwrap();
        assert_eq!(next.purpose, "verify");
        assert_eq!(f.store.task(&task.id).unwrap().reworks_used, 0);
        assert_eq!(
            f.store.task(&task.id).unwrap().runs_used,
            task.runs_used + 1
        );
    }
}

#[test]
fn cancelling_task_supersedes_open_blocker_and_rejects_late_resolution() {
    let mut f = Fixture::new(false);
    let (run, bound, _) = f.running_executor();
    let report = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &run,
                "report",
                "task_report_blocker",
                json!({"handler":f.human,"reason":"取消分支"}),
            ),
        )
        .unwrap();
    let id = report["data"]["blocker"]["id"].as_str().unwrap();
    f.store
        .execute(
            "cancel",
            &Command::TaskCancel {
                id: run.task_id.clone(),
                revision: run.task_revision,
                reason: "本人取消".into(),
            },
        )
        .unwrap();
    f.fix_run(&run);
    assert_eq!(f.store.blocker(id).unwrap().state, "superseded");
    let task = f.store.task(&run.task_id).unwrap();
    assert_eq!(task.state, "closed");
    assert!(
        f.store
            .execute(
                "late-resolution",
                &Command::BlockerResolve {
                    id: id.into(),
                    revision: 2,
                    task_revision: task.revision,
                    evidence: "迟到修复不能恢复已取消任务".into()
                }
            )
            .is_err()
    );
}

#[test]
fn designated_digital_coordinator_resolves_blocker_then_explicitly_arranges_rework() {
    let mut f = Fixture::new(true);
    let executor = f.prepare_executor();
    let (lead, bound) = f.running_member_with_contract(Some(generic_contract()));
    assert_eq!(
        f.store
            .member_call(
                &bound,
                &member_operation(
                    &lead,
                    "accept",
                    "task_intake",
                    json!({"revision":2,"decision":"accept","reason":"契约完整"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let arranged = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &lead,
                "arrange",
                "task_arrange",
                json!({"revision":3,"action":"execute","instruction":"执行原契约"}),
            ),
        )
        .unwrap();
    assert_eq!(arranged["ok"], true, "{arranged}");
    assert_eq!(f.store.member_call(&bound,&member_operation(&lead,"respond","message_respond",json!({"kind":"assignment","messageId":arranged["data"]["delivery"]["messageId"]}))).unwrap()["ok"],true);
    f.store
        .runtime_run_observed_stopped("service", &lead.id, "fixture leader stopped")
        .unwrap();
    let exec = f
        .store
        .runtime_claim(
            "service",
            arranged["data"]["delivery"]["deliveryId"].as_str().unwrap(),
            &executor,
        )
        .unwrap();
    f.store.runtime_begin_launch("service", &exec.id).unwrap();
    let exec = f
        .store
        .runtime_child_started("service", &exec.id, 321, "fixture-blocked-member")
        .unwrap();
    let bound = f.store.bind_member("service", &exec.id).unwrap();
    let report = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &exec,
                "report",
                "task_report_blocker",
                json!({"handler":lead.worker_id,"reason":"需要协调原范围内依赖，未产生文件"}),
            ),
        )
        .unwrap();
    assert_eq!(report["ok"], true, "{report}");
    let id = report["data"]["blocker"]["id"].as_str().unwrap();
    f.store
        .runtime_run_observed_stopped(
            "service",
            &exec.id,
            "fixture execution resources confirmed stopped",
        )
        .unwrap();
    let mailbox = f.store.mailbox(Some(&lead.worker_id)).unwrap();
    let delivery = mailbox
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["message"]["kind"] == "blocker")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let coord = f
        .store
        .runtime_claim("service", delivery, &lead.configuration_id)
        .unwrap();
    f.store.runtime_begin_launch("service", &coord.id).unwrap();
    let coord = f
        .store
        .runtime_child_started("service", &coord.id, 321, "fixture-resolver")
        .unwrap();
    let bound = f.store.bind_member("service", &coord.id).unwrap();
    let operation = member_operation(
        &coord,
        "resolve",
        "blocker_resolve",
        json!({"id":id,"revision":1,"taskRevision":3,"evidence":"原范围内依赖已补齐，未修改契约"}),
    );
    let result = f.store.member_call(&bound, &operation).unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["data"]["state"], "resolved");
    assert_eq!(f.store.member_call(&bound, &operation).unwrap(), result);
    assert_eq!(
        f.store.task_details(&coord.task_id).unwrap()["rework_arrangements"],
        json!([])
    );
    let arranged=f.store.member_call(&bound,&member_operation(&coord,"rework","task_arrange",json!({"revision":3,"action":"rework","reason":{"kind":"blocker","id":id},"instruction":"依赖就绪，继续原任务"}))).unwrap();
    assert_eq!(arranged["ok"], true, "{arranged}");
    assert_eq!(f.store.member_call(&bound,&member_operation(&coord,"finish","message_respond",json!({"kind":"assignment","messageId":arranged["data"]["delivery"]["messageId"]}))).unwrap()["ok"],true);
    f.store
        .runtime_run_observed_stopped("service", &coord.id, "fixture coordinator stopped")
        .unwrap();
    let next = f
        .store
        .runtime_claim(
            "service",
            arranged["data"]["delivery"]["deliveryId"].as_str().unwrap(),
            &executor,
        )
        .unwrap();
    assert_eq!(next.purpose, "rework");
    assert_eq!(f.store.task(&coord.task_id).unwrap().reworks_used, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn real_typescript_pipe_blocker_requests_stop_and_waits_for_child_reaping() {
    channel_fixture("blocker").await;
}

// These fixtures exercise acceptance transactions, not real check execution.
impl Fixture {
    fn acceptance_fixture(&mut self) -> (Run, atelier::verification::Verification) {
        self.verification_fixture("pass")
    }
    fn verification_fixture(
        &mut self,
        recommendation: &str,
    ) -> (Run, atelier::verification::Verification) {
        let (run, binding) = self.code_artifact_for_check(
            "<h1>synthetic acceptance candidate</h1>",
            &format!("sha256:{}", "a".repeat(64)),
        );
        let task = self.store.task(&run.task_id).unwrap();
        let artifact = self
            .store
            .artifact(task.current_artifact.as_deref().unwrap())
            .unwrap();
        let accepted = self.store.member_call(&binding, &member_operation(&run, "accept", "handoff_respond", json!({"id":artifact.handoff_id,"revision":1,"accept":true,"reason":"fixture handoff"}))).unwrap();
        assert_eq!(accepted["ok"], true, "{accepted}");
        let check = self
            .store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "check",
                    "run_check",
                    json!({"checkId":"tic-tac-toe-browser-v1"}),
                ),
            )
            .unwrap();
        let sql = rusqlite::Connection::open(self.dir.path().join("atelier.sqlite3")).unwrap();
        sql.execute("UPDATE checks SET state='finished',data=json_set(data,'$.state','finished','$.conclusion','pass','$.resources_stopped',json('true')) WHERE id=?1", [check["data"]["check"]["id"].as_str().unwrap()]).unwrap();
        let verified = self.store.member_call(&binding, &member_operation(&run, "verify", "verification_submit", json!({"evidenceId":check["data"]["check"]["id"],"recommendation":recommendation,"reason":"synthetic evidence for lifecycle transaction tests only"}))).unwrap();
        assert_eq!(verified["ok"], true, "{verified}");
        (
            run,
            serde_json::from_value(verified["data"].clone()).unwrap(),
        )
    }
    fn acceptance_request(&mut self, v: &atelier::verification::Verification) -> Value {
        self.store
            .execute("acceptance-request", &acceptance_request_command(v))
            .unwrap()
    }
}
fn acceptance_request_command(v: &atelier::verification::Verification) -> Command {
    Command::AcceptanceRequest {
        task_id: v.task_id.clone(),
        revision: v.task_revision,
        artifact_id: v.artifact_id.clone(),
        verification_id: v.id.clone(),
        summary: "请本人审阅交付".into(),
    }
}
fn acceptance_decide(
    v: &atelier::verification::Verification,
    request: &Value,
    accept: bool,
) -> Command {
    Command::AcceptanceDecide {
        task_id: v.task_id.clone(),
        revision: v.task_revision,
        request_id: request["decision"]["id"].as_str().unwrap().into(),
        request_revision: 1,
        accept,
        reason: if accept {
            "本人确认符合约定"
        } else {
            "需补充交付说明"
        }
        .into(),
        decision_ref: Some("synthetic-human-decision".into()),
    }
}

#[test]
fn acceptance_requires_stopped_verifier_and_closes_atomically_with_durable_record() {
    let mut f = Fixture::new(false);
    let (run, v) = f.acceptance_fixture();
    assert!(
        f.store
            .execute("premature", &acceptance_request_command(&v))
            .is_err()
    );
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture resources stopped")
        .unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_acceptance_request BEFORE INSERT ON decisions WHEN json_extract(NEW.data,'$.kind')='acceptance' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let before = f.store.mailbox(Some(&f.human)).unwrap();
    assert!(
        f.store
            .execute("acceptance-request", &acceptance_request_command(&v))
            .is_err()
    );
    assert_eq!(f.store.mailbox(Some(&f.human)).unwrap(), before);
    sql.execute_batch("DROP TRIGGER fail_acceptance_request;")
        .unwrap();
    let request = f.acceptance_request(&v);
    assert_eq!(f.store.task(&v.task_id).unwrap().state, "active");
    assert!(
        f.store
            .execute("duplicate", &acceptance_request_command(&v))
            .is_err()
    );
    let id = request["decision"]["id"].as_str().unwrap();
    assert!(
        f.store
            .execute(
                "ordinary-response",
                &Command::DecisionRespond {
                    id: id.into(),
                    revision: 1,
                    answer: "accept".into()
                }
            )
            .is_err()
    );
    sql.execute_batch("CREATE TRIGGER fail_acceptance_close BEFORE UPDATE ON tasks WHEN json_extract(NEW.data,'$.state')='closed' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let command = acceptance_decide(&v, &request, true);
    assert!(f.store.execute("acceptance-decision", &command).is_err());
    assert!(f.store.acceptance_decision(id).is_err());
    assert_eq!(f.store.decision(id).unwrap().state, "open");
    assert_eq!(f.store.task(&v.task_id).unwrap().state, "active");
    sql.execute_batch("DROP TRIGGER fail_acceptance_close;")
        .unwrap();
    let result = f.store.execute("acceptance-decision", &command).unwrap();
    assert_eq!(result["task"]["state"], "closed");
    assert_eq!(result["task"]["outcome"], "accepted");
    assert_eq!(result["task"]["revision"], v.task_revision + 1);
    assert_eq!(
        f.store.execute("acceptance-decision", &command).unwrap(),
        result
    );
    assert!(
        f.store
            .execute("late-rejection", &acceptance_decide(&v, &request, false))
            .is_err()
    );
    let reopened = Store::open(f.dir.path()).unwrap();
    assert!(reopened.acceptance_decision(id).unwrap().accepted);
    assert_eq!(reopened.decision(id).unwrap().state, "accepted");
    assert_eq!(reopened.task(&v.task_id).unwrap().state, "closed");
}

#[test]
fn acceptance_rejection_is_atomic_and_requires_new_artifact_before_another_request() {
    let mut f = Fixture::new(false);
    let (run, v) = f.acceptance_fixture();
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture stopped")
        .unwrap();
    let request = f.acceptance_request(&v);
    let id = request["decision"]["id"].as_str().unwrap();
    let command = acceptance_decide(&v, &request, false);
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_rejection_notification BEFORE INSERT ON messages WHEN NEW.kind='decision.result' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.store.execute("reject", &command).is_err());
    assert!(f.store.acceptance_decision(id).is_err());
    assert_eq!(f.store.decision(id).unwrap().state, "open");
    sql.execute_batch("DROP TRIGGER fail_rejection_notification;")
        .unwrap();
    let result = f.store.execute("reject", &command).unwrap();
    assert_eq!(result["decision"]["state"], "rejected");
    assert_eq!(result["task"]["state"], "active");
    assert!(!f.store.acceptance_decision(id).unwrap().accepted);
    assert_eq!(f.store.execute("reject", &command).unwrap(), result);
    assert!(
        f.store
            .execute("switch-to-accept", &acceptance_decide(&v, &request, true))
            .is_err()
    );
    assert!(
        f.store
            .execute("request-again", &acceptance_request_command(&v))
            .is_err()
    );
    assert_eq!(
        f.store.task_details(&v.task_id).unwrap()["rework_arrangements"],
        json!([])
    );
    let rework = f
        .store
        .execute(
            "explicit-rework",
            &Command::TaskRework {
                id: v.task_id.clone(),
                revision: v.task_revision,
                reason: atelier::rework::ReworkReason::Rejection { id: id.into() },
                instruction: "补充交付说明，重新独立检验".into(),
            },
        )
        .unwrap();
    assert_eq!(rework["rework"]["baseArtifact"], v.artifact_id);
    assert_eq!(rework["budget"]["reworksReserved"], 1);
}

#[test]
fn acceptance_rechecks_current_permission_and_cancellation_supersedes_open_request() {
    let mut f = Fixture::new(false);
    let (run, v) = f.acceptance_fixture();
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture stopped")
        .unwrap();
    let request = f.acceptance_request(&v);
    let team = f.store.team(&f.team.id).unwrap();
    f.store
        .execute(
            "revoke-accept",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: team.id,
                revision: team.revision,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(f.human.clone(), vec![Permission::Accept])]),
            },
        )
        .unwrap();
    assert!(
        f.store
            .execute("revoked", &acceptance_decide(&v, &request, true))
            .is_err()
    );
    assert_eq!(f.store.task(&v.task_id).unwrap().state, "active");
    f.store
        .execute(
            "cancel",
            &Command::TaskCancel {
                id: v.task_id.clone(),
                revision: v.task_revision,
                reason: "本人取消".into(),
            },
        )
        .unwrap();
    let id = request["decision"]["id"].as_str().unwrap();
    assert_eq!(f.store.decision(id).unwrap().state, "superseded");
    assert!(f.store.acceptance_decision(id).is_err());
}

#[test]
fn acceptance_rejects_invalid_or_unmatched_independent_evidence() {
    let mut f = Fixture::new(false);
    let (run, v) = f.acceptance_fixture();
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture stopped")
        .unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    for (index, (table, id, field, value)) in [
        ("verifications", &v.id, "$.conclusion", json!("fail")),
        (
            "verifications",
            &v.id,
            "$.conclusion",
            json!("inconclusive"),
        ),
        ("verifications", &v.id, "$.task_revision", json!(1)),
        (
            "verifications",
            &v.id,
            "$.worker_id",
            json!(f.team.executor),
        ),
        (
            "checks",
            &v.check_id,
            "$.target",
            json!({"kind":"candidate","run_id":run.id,"revision":1}),
        ),
        ("checks", &v.check_id, "$.resources_stopped", json!(false)),
        ("artifacts", &v.artifact_id, "$.partial", json!(true)),
        ("tasks", &v.task_id, "$.current_artifact", json!("outdated")),
    ]
    .into_iter()
    .enumerate()
    {
        let original: String = sql
            .query_row(
                &format!("SELECT data FROM {table} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap();
        sql.execute(
            &format!("UPDATE {table} SET data=json_set(data,?2,json(?3)) WHERE id=?1"),
            rusqlite::params![id, field, value.to_string()],
        )
        .unwrap();
        assert!(
            f.store
                .execute(&format!("invalid-{index}"), &acceptance_request_command(&v))
                .is_err(),
            "{table} {field}"
        );
        sql.execute(
            &format!("UPDATE {table} SET data=?2 WHERE id=?1"),
            rusqlite::params![id, original],
        )
        .unwrap();
    }
    assert_eq!(f.acceptance_request(&v)["decision"]["state"], "open");
}

#[test]
fn digital_leader_requests_acceptance_and_processes_rejection_through_mailbox() {
    let mut f = Fixture::new(true);
    let (verify, v) = f.acceptance_fixture();
    f.store
        .runtime_run_observed_stopped("service", &verify.id, "fixture verifier stopped")
        .unwrap();
    let task = f.store.task(&v.task_id).unwrap();
    let config = task.worker_snapshots[&f.team.leader]
        .execution_config
        .as_deref()
        .unwrap();
    let mail = f.store.mailbox(Some(&f.team.leader)).unwrap();
    let delivery = mail
        .as_array()
        .unwrap()
        .iter()
        .find(|d| {
            d["message"]["body"]
                .as_str()
                .is_some_and(|b| b.contains(&v.id))
        })
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let run = f.store.runtime_claim("service", delivery, config).unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-acceptance-requester")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    assert!(
        f.store
            .execute(
                "human-cannot-replace-leader",
                &acceptance_request_command(&v)
            )
            .is_err()
    );
    let result=f.store.member_call(&binding,&member_operation(&run,"request","acceptance_request",json!({"revision":v.task_revision,"artifactId":v.artifact_id,"verificationId":v.id,"summary":"独立检验已通过，请本人决定"}))).unwrap();
    assert_eq!(result["ok"], true, "{result}");
    let request = &result["data"];
    let id = request["decision"]["id"].as_str().unwrap();
    assert!(
        f.store
            .execute(
                "while-leader-running",
                &acceptance_decide(&v, request, true)
            )
            .is_err()
    );
    let forged = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "forge-accept",
                "acceptance_decide",
                json!({"requestId":id,"accept":true}),
            ),
        )
        .unwrap();
    assert_eq!(forged["ok"], false, "{forged}");
    assert_eq!(
        f.store
            .member_call(
                &binding,
                &member_operation(
                    &run,
                    "finish",
                    "message_respond",
                    json!({"kind":"decision","decisionId":id})
                )
            )
            .unwrap()["ok"],
        true
    );
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture requester stopped")
        .unwrap();
    let rejected = f
        .store
        .execute("human-reject", &acceptance_decide(&v, request, false))
        .unwrap();
    let delivery = rejected["decision"]["response_delivery"].as_str().unwrap();
    let run = f.store.runtime_claim("service", delivery, config).unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-rejection-handler")
        .unwrap();
    let binding = f.store.bind_member("service", &run.id).unwrap();
    let arranged=f.store.member_call(&binding,&member_operation(&run,"rework","task_arrange",json!({"revision":v.task_revision,"action":"rework","reason":{"kind":"rejection","id":id},"instruction":"根据本人拒绝补充新版交付"}))).unwrap();
    assert_eq!(arranged["ok"], true, "{arranged}");
    let done = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "finish",
                "message_respond",
                json!({"kind":"assignment","messageId":arranged["data"]["delivery"]["messageId"]}),
            ),
        )
        .unwrap();
    assert_eq!(done["ok"], true, "{done}");
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture coordinator stopped")
        .unwrap();
    assert_eq!(f.store.task(&v.task_id).unwrap().state, "active");
    assert_eq!(f.store.decision(id).unwrap().state, "rejected");
}

fn repaired_tic_tac_toe() -> String {
    include_str!("../samples/tic-tac-toe/index.html").replace(
        "[[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8]]",
        "[[0,1,2],[3,4,5],[6,7,8],[0,3,6],[1,4,7],[2,5,8],[0,4,8],[2,4,6]]",
    )
}
fn acceptance_cli(f: &Fixture, request_id: &str, args: &[&str]) -> Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(f.dir.path())
        .args(["--json", "--request-id", request_id])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn request_acceptance_cli(
    f: &Fixture,
    task: &Task,
    artifact: &str,
    verification: &str,
    operation: &str,
) -> Value {
    acceptance_cli(
        f,
        operation,
        &[
            "task",
            "acceptance",
            "request",
            &task.id,
            "--revision",
            &task.revision.to_string(),
            "--artifact",
            artifact,
            "--verification",
            verification,
            "--summary",
            "实际独立检查已通过，请本人决定",
        ],
    )
}
fn decide_acceptance_cli(
    f: &Fixture,
    task: &Task,
    request: &Value,
    accept: bool,
    operation: &str,
) -> Value {
    acceptance_cli(
        f,
        operation,
        &[
            "task",
            if accept { "accept" } else { "reject" },
            &task.id,
            "--revision",
            &task.revision.to_string(),
            "--request",
            request["data"]["decision"]["id"].as_str().unwrap(),
            "--request-revision",
            "1",
            "--reason",
            if accept {
                "本人已检查新版符合约定"
            } else {
                "要求补充交付说明"
            },
            "--decision-ref",
            "fixture-human-decision-not-product-acceptance",
        ],
    )
}

#[test]
fn artifact_export_cli_preserves_partial_snapshot_and_never_overwrites_or_accepts() {
    let mut f = Fixture::new(false);
    let (run, _, candidate) = f.running_executor();
    std::fs::create_dir(candidate.join("nested")).unwrap();
    std::fs::write(candidate.join("nested/data.bin"), [0, 255, 42]).unwrap();
    std::fs::write(candidate.join("launch.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        candidate.join("launch.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let artifact = f.fix_run(&run);
    assert!(artifact.partial);
    let before = f.store.task_details(&run.task_id).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let output = outside.path().join("export");
    std::fs::create_dir(&output).unwrap();
    let exported = acceptance_cli(
        &f,
        "export-files",
        &[
            "artifact",
            "export",
            &artifact.id,
            "--destination",
            output.to_str().unwrap(),
        ],
    );
    assert_eq!(exported["data"]["partial"], true);
    assert_eq!(exported["data"]["contentDigest"], artifact.content_digest);
    assert_eq!(exported["data"]["acceptanceChanged"], false);
    assert_eq!(
        std::fs::read(output.join("nested/data.bin")).unwrap(),
        [0, 255, 42]
    );
    assert_ne!(
        std::fs::metadata(output.join("launch.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o100,
        0
    );
    assert!(!output.join("atelier.sqlite3").exists());
    assert!(!output.join("objects").exists());
    assert!(f.store.export_artifact(&artifact.id, &output).is_err());
    assert_eq!(
        std::fs::read(output.join("nested/data.bin")).unwrap(),
        [0, 255, 42]
    );
    assert!(
        f.store
            .export_artifact(&artifact.id, &f.dir.path().join("internal"))
            .is_err()
    );
    let link = outside.path().join("linked");
    std::os::unix::fs::symlink(&output, &link).unwrap();
    assert!(f.store.export_artifact(&artifact.id, &link).is_err());
    std::fs::write(
        candidate.join("index.html"),
        "later candidate changes are not this artifact",
    )
    .unwrap();
    let second = outside.path().join("second");
    f.store.export_artifact(&artifact.id, &second).unwrap();
    assert_eq!(
        std::fs::read(second.join("index.html")).unwrap(),
        std::fs::read(output.join("index.html")).unwrap()
    );
    assert_eq!(f.store.task_details(&run.task_id).unwrap(), before);
}

#[test]
fn artifact_export_validates_all_blobs_before_publishing_and_rejects_damaged_manifest() {
    let mut f = Fixture::new(false);
    let (run, _, _) = f.running_executor();
    let artifact = f.fix_run(&run);
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("export");
    let blob = f
        .dir
        .path()
        .join("objects/blobs")
        .join(&artifact.files[0].sha256);
    let original = std::fs::read(&blob).unwrap();
    std::fs::write(&blob, "corrupt").unwrap();
    assert!(f.store.export_artifact(&artifact.id, &target).is_err());
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    std::fs::write(&blob, original).unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute(
        "UPDATE artifacts SET data=json_set(data,'$.files[0].path','../escape') WHERE id=?1",
        [&artifact.id],
    )
    .unwrap();
    assert!(f.store.export_artifact(&artifact.id, &target).is_err());
    assert!(!target.exists());
}

#[test]
fn inconclusive_verification_requires_new_handoff_and_keeps_original_terminal_history() {
    let mut f = Fixture::new(false);
    let (run, v) = f.verification_fixture("inconclusive");
    assert_eq!(v.conclusion, "inconclusive");
    let task = f.store.task(&v.task_id).unwrap();
    let artifact = f.store.artifact(&v.artifact_id).unwrap();
    let old_id = artifact.handoff_id.as_deref().unwrap();
    let command = Command::TaskVerify {
        verification_id: Some(v.id.clone()),
        blocker_id: None,
        id: v.task_id.clone(),
        revision: v.task_revision,
        artifact_id: v.artifact_id.clone(),
        instruction: "原检查无法得出结论，检查环境已核对，请独立重验".into(),
    };
    assert!(f.store.execute("before-stop", &command).is_err());
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture verifier resources stopped")
        .unwrap();
    assert!(
        f.store
            .execute("accept-inconclusive", &acceptance_request_command(&v))
            .is_err()
    );
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_recheck BEFORE INSERT ON messages WHEN NEW.kind='handoff.verify' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.store.execute("recheck", &command).is_err());
    assert_eq!(f.store.handoff(old_id).unwrap().state, "accepted");
    assert_eq!(f.store.handoff(old_id).unwrap().revision, 2);
    sql.execute_batch("DROP TRIGGER fail_recheck;").unwrap();
    let offered = acceptance_cli(
        &f,
        "recheck",
        &[
            "task",
            "verify",
            &v.task_id,
            "--revision",
            &v.task_revision.to_string(),
            "--artifact",
            &v.artifact_id,
            "--inconclusive",
            &v.id,
            "--instruction",
            "环境已核对，重新独立检验",
        ],
    );
    let old = f.store.handoff(old_id).unwrap();
    assert_eq!(old.state, "superseded");
    assert_eq!(old.prior_state.as_deref(), Some("accepted"));
    assert_eq!(old.reason.as_deref(), Some("fixture handoff"));
    assert_eq!(
        old.superseded_by_verification.as_deref(),
        Some(v.id.as_str())
    );
    assert_eq!(
        f.store.verification(&v.id).unwrap().conclusion,
        "inconclusive"
    );
    assert!(f.store.execute("duplicate-recheck", &command).is_err());
    let reopened = Store::open(f.dir.path()).unwrap();
    assert_eq!(reopened.handoff(old_id).unwrap().state, "superseded");
    let next_id = offered["data"]["handoff"]["id"].as_str().unwrap();
    let (next, bound) = f.claim_handoff(next_id, &run.configuration_id);
    assert_ne!(next.delivery_id, run.delivery_id);
    assert_eq!(
        f.store.task(&v.task_id).unwrap().runs_used,
        task.runs_used + 1
    );
    assert_eq!(f.store.task(&v.task_id).unwrap().reworks_used, 0);
    let denied = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &next,
                "before-accept",
                "run_check",
                json!({"checkId":"tic-tac-toe-browser-v1"}),
            ),
        )
        .unwrap();
    assert_eq!(denied["ok"], false);
    let accepted = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &next,
                "accept",
                "handoff_respond",
                json!({"id":next_id,"revision":1,"accept":true,"reason":"重新接收"}),
            ),
        )
        .unwrap();
    assert_eq!(accepted["ok"], true, "{accepted}");
    let stale=f.store.member_call(&bound,&member_operation(&next,"reuse-old-check","verification_submit",json!({"evidenceId":v.check_id,"recommendation":"pass","reason":"旧证据不属于新 Run"}))).unwrap();
    assert_eq!(stale["ok"], false);
    let fresh = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &next,
                "new-check",
                "run_check",
                json!({"checkId":"tic-tac-toe-browser-v1"}),
            ),
        )
        .unwrap();
    assert_eq!(fresh["ok"], true, "{fresh}");
    assert_ne!(fresh["data"]["check"]["id"], v.check_id);
}

#[test]
fn recheck_cannot_use_pass_or_fail_as_inconclusive_or_bypass_conflicting_reasons() {
    for conclusion in ["pass", "fail", "inconclusive"] {
        let mut f = Fixture::new(false);
        let (run, v) = f.verification_fixture(conclusion);
        f.store
            .runtime_run_observed_stopped("service", &run.id, "fixture stopped")
            .unwrap();
        let command = Command::TaskVerify {
            verification_id: Some(v.id.clone()),
            blocker_id: if conclusion == "inconclusive" {
                Some("another-reason".into())
            } else {
                None
            },
            id: v.task_id.clone(),
            revision: v.task_revision,
            artifact_id: v.artifact_id.clone(),
            instruction: "不允许将明确通过或失败作为不确定重检，原因也不能混用".into(),
        };
        assert!(f.store.execute("invalid-recheck", &command).is_err());
        assert_eq!(
            f.store
                .handoff(
                    f.store
                        .artifact(&v.artifact_id)
                        .unwrap()
                        .handoff_id
                        .as_deref()
                        .unwrap()
                )
                .unwrap()
                .state,
            "accepted"
        );
        assert_eq!(f.store.task(&v.task_id).unwrap().reworks_used, 0);
    }
}

#[test]
fn digital_leader_explicitly_arranges_recheck_after_inconclusive_result() {
    let mut f = Fixture::new(true);
    let (verify, v) = f.verification_fixture("inconclusive");
    f.store
        .runtime_run_observed_stopped("service", &verify.id, "fixture verifier stopped")
        .unwrap();
    let task = f.store.task(&v.task_id).unwrap();
    let config = task.worker_snapshots[&f.team.leader]
        .execution_config
        .as_deref()
        .unwrap();
    let mail = f.store.mailbox(Some(&f.team.leader)).unwrap();
    let delivery = mail
        .as_array()
        .unwrap()
        .iter()
        .find(|d| {
            d["message"]["body"]
                .as_str()
                .is_some_and(|b| b.contains(&v.id))
        })
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let run = f.store.runtime_claim("service", delivery, config).unwrap();
    f.store.runtime_begin_launch("service", &run.id).unwrap();
    let run = f
        .store
        .runtime_child_started("service", &run.id, 321, "fixture-recheck-leader")
        .unwrap();
    let bound = f.store.bind_member("service", &run.id).unwrap();
    let op = member_operation(
        &run,
        "recheck",
        "task_arrange",
        json!({"revision":task.revision,"action":"verify","artifactId":v.artifact_id,"verificationId":v.id,"instruction":"不确定检验已结束，重新交接"}),
    );
    let offered = f.store.member_call(&bound, &op).unwrap();
    assert_eq!(offered["ok"], true, "{offered}");
    assert_eq!(f.store.member_call(&bound, &op).unwrap(), offered);
    let done = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &run,
                "finish",
                "message_respond",
                json!({"kind":"assignment","messageId":offered["data"]["delivery"]["messageId"]}),
            ),
        )
        .unwrap();
    assert_eq!(done["ok"], true, "{done}");
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture leader stopped")
        .unwrap();
    assert_eq!(f.store.task(&task.id).unwrap().state, "active");
    assert_eq!(f.store.task(&task.id).unwrap().reworks_used, 0);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires prepared immutable Docker image; real CLI recheck, fixture member recommendations"]
async fn real_docker_inconclusive_verification_new_handoff_and_fresh_evidence() {
    let image = "sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133";
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".agents/verify-runs/1");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("recheck-")
        .tempdir_in(&root)
        .unwrap();
    let mut f = Fixture::in_directory(false, dir);
    let (run, bound) = f
        .code_artifact_through_member_tools(&repaired_tic_tac_toe(), image)
        .await;
    let task = f.store.task(&run.task_id).unwrap();
    let artifact = f
        .store
        .artifact(task.current_artifact.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        f.store
            .member_call(
                &bound,
                &member_operation(
                    &run,
                    "accept",
                    "handoff_respond",
                    json!({"id":artifact.handoff_id,"revision":1,"accept":true,"reason":"资料完整"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let database = Database::open(f.dir.path().into(), 8).await.unwrap();
    let client = database.client();
    let check = client
        .member_call(
            bound.clone(),
            member_operation(
                &run,
                "check",
                "run_check",
                json!({"checkId":"tic-tac-toe-browser-v1"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(check["data"]["check"]["conclusion"], "pass", "{check}");
    // Real check passes; the fixture member deliberately withholds a pass recommendation.
    let inconclusive=client.member_call(bound,member_operation(&run,"verification","verification_submit",json!({"evidenceId":check["data"]["check"]["id"],"recommendation":"inconclusive","reason":"fixture 成员对结论仍有疑问，不代表真实检查故障"}))).await.unwrap();
    assert_eq!(
        inconclusive["data"]["conclusion"], "inconclusive",
        "{inconclusive}"
    );
    f.store
        .runtime_run_observed_stopped(
            "service",
            &run.id,
            "fixture process stopped after real Docker check",
        )
        .unwrap();
    let id = inconclusive["data"]["id"].as_str().unwrap();
    let offered = acceptance_cli(
        &f,
        "recheck",
        &[
            "task",
            "verify",
            &task.id,
            "--revision",
            &task.revision.to_string(),
            "--artifact",
            &artifact.id,
            "--inconclusive",
            id,
            "--instruction",
            "重新独立核对全部规则",
        ],
    );
    let (next, bound) = f.claim_handoff(
        offered["data"]["handoff"]["id"].as_str().unwrap(),
        &run.configuration_id,
    );
    assert_eq!(f.store.member_call(&bound,&member_operation(&next,"accept","handoff_respond",json!({"id":offered["data"]["handoff"]["id"],"revision":1,"accept":true,"reason":"重新接收"}))).unwrap()["ok"],true);
    let fresh = client
        .member_call(
            bound.clone(),
            member_operation(
                &next,
                "check",
                "run_check",
                json!({"checkId":"tic-tac-toe-browser-v1"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(fresh["data"]["check"]["conclusion"], "pass", "{fresh}");
    assert_ne!(fresh["data"]["check"]["id"], check["data"]["check"]["id"]);
    assert_eq!(
        fresh["data"]["check"]["target"],
        check["data"]["check"]["target"]
    );
    let passed=client.member_call(bound,member_operation(&next,"verification","verification_submit",json!({"evidenceId":fresh["data"]["check"]["id"],"recommendation":"pass","reason":"重新核对完成"}))).await.unwrap();
    assert_eq!(passed["data"]["conclusion"], "pass", "{passed}");
    f.store
        .runtime_run_observed_stopped(
            "service",
            &next.id,
            "fixture verifier stopped after fresh check",
        )
        .unwrap();
    assert_eq!(f.store.verification(id).unwrap().conclusion, "inconclusive");
    assert_eq!(f.store.task(&task.id).unwrap().reworks_used, 0);
    assert_eq!(
        f.store.task(&task.id).unwrap().runs_used,
        task.runs_used + 1
    );
    let request = request_acceptance_cli(
        &f,
        &task,
        &artifact.id,
        passed["data"]["id"].as_str().unwrap(),
        "request",
    );
    let accepted = decide_acceptance_cli(&f, &task, &request, true, "final-human-acceptance");
    assert_eq!(accepted["data"]["task"]["state"], "closed");
    let path = root.join(format!("recheck-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path,serde_json::to_vec_pretty(&json!({"productAcceptance":false,"memberFixture":true,"realDocker":true,"realCli":true,"check":check,"inconclusive":inconclusive,"newHandoff":offered,"freshCheck":fresh,"passed":passed,"accepted":accepted})).unwrap()).unwrap();
    println!("Recheck evidence: {}", path.display());
}

fn fixture_delivery(f: &Fixture, worker: &str, id: &str) -> Value {
    f.store
        .mailbox(Some(worker))
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap()
        .clone()
}
#[test]
fn mailbox_retry_continues_advanced_intake_without_replaying_committed_operations() {
    let mut f = Fixture::new(true);
    let (run, bound) = f.running_member_with_contract(Some(generic_contract()));
    let intake = member_operation(
        &run,
        "accept",
        "task_intake",
        json!({"revision":2,"decision":"accept","reason":"已完成承接"}),
    );
    let accepted = f.store.member_call(&bound, &intake).unwrap();
    assert_eq!(accepted["ok"], true);
    let op = member_operation(
        &run,
        "note",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"已完成承接，继续安排"}),
    );
    let sent = f.store.member_call(&bound, &op).unwrap();
    assert_eq!(sent["ok"], true);
    f.store
        .runtime_run_observed_stopped(
            "service",
            &run.id,
            "fixture crash after committed operations",
        )
        .unwrap();
    choose_recovery_retry(&mut f, &run.task_id);
    let original = fixture_delivery(&f, &run.worker_id, &run.delivery_id);
    assert_eq!(original["status"], "blocked");
    let task = f.store.task(&run.task_id).unwrap();
    assert_eq!(task.revision, 3);
    assert_eq!(original["message"]["taskRevision"], 2);
    let cmd = Command::MailboxRetry {
        id: run.delivery_id.clone(),
        revision: original["revision"].as_u64().unwrap(),
        reason: "原配置已核对，继续当前责任".into(),
    };
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_retry BEFORE INSERT ON requests WHEN NEW.id='retry-rollback' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.store.execute("retry-rollback", &cmd).is_err());
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "blocked"
    );
    sql.execute_batch("DROP TRIGGER fail_retry;").unwrap();
    let queued = acceptance_cli(
        &f,
        "retry",
        &[
            "mailbox",
            "retry",
            &run.delivery_id,
            "--revision",
            &original["revision"].to_string(),
            "--reason",
            "原配置已核对，继续当前责任",
        ],
    );
    assert_eq!(queued["data"]["delivery"]["status"], "queued");
    assert_eq!(f.store.task(&task.id).unwrap().runs_used, task.runs_used);
    let next = f
        .store
        .runtime_claim("service", &run.delivery_id, &run.configuration_id)
        .unwrap();
    assert_eq!(next.task_revision, 3);
    assert_ne!(next.id, run.id);
    f.store.runtime_begin_launch("service", &next.id).unwrap();
    let next = f
        .store
        .runtime_child_started("service", &next.id, 321, "fixture resumed coordinator")
        .unwrap();
    let binding = f.store.bind_member("service", &next.id).unwrap();
    let messages = f.store.task(&task.id).unwrap().messages_used;
    assert_eq!(f.store.member_call(&binding, &intake).unwrap(), accepted);
    assert_eq!(f.store.member_call(&binding, &op).unwrap(), sent);
    let never_submitted = member_operation(
        &run,
        "never-submitted-before-crash",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"不能在新 Run 补执行"}),
    );
    assert!(matches!(
        f.store.member_call(&binding, &never_submitted),
        Err(Error::Conflict(_))
    ));
    assert_eq!(f.store.task(&task.id).unwrap().revision, 3);
    assert_eq!(f.store.task(&task.id).unwrap().messages_used, messages);
    assert_eq!(
        f.store.task(&task.id).unwrap().runs_used,
        task.runs_used + 1
    );
    let wait = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &next,
                "finish",
                "message_respond",
                json!({"kind":"wait","reason":"明确保留后续安排责任","handler":run.worker_id}),
            ),
        )
        .unwrap();
    assert_eq!(wait["ok"], true, "{wait}");
    f.store
        .runtime_run_observed_stopped("service", &next.id, "fixture resumed process stopped")
        .unwrap();
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "handled"
    );
}

#[test]
fn mailbox_retry_does_not_release_unknown_reset_budgets_or_repeat_code_execution() {
    let mut f = Fixture::new(true);
    let (run, _) = f.running_member_with_contract(Some(generic_contract()));
    f.store.runtime_register("recovery", 456).unwrap();
    choose_recovery_retry(&mut f, &run.task_id);
    let d = fixture_delivery(&f, &run.worker_id, &run.delivery_id);
    assert_eq!(d["status"], "uncertain");
    assert!(
        f.store
            .execute(
                "unknown-retry",
                &Command::MailboxRetry {
                    id: run.delivery_id.clone(),
                    revision: d["revision"].as_u64().unwrap(),
                    reason: "无停止证据".into()
                }
            )
            .is_err()
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    f.store
        .runtime_run_observed_stopped("recovery", &run.id, "fixture trusted stop proof")
        .unwrap();
    let d = fixture_delivery(&f, &run.worker_id, &run.delivery_id);
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute("UPDATE tasks SET data=json_set(data,'$.runs_used',json_extract(data,'$.contract.max_runs')) WHERE id=?1",[&run.task_id]).unwrap();
    assert!(
        f.store
            .execute(
                "exhausted-retry",
                &Command::MailboxRetry {
                    id: run.delivery_id.clone(),
                    revision: d["revision"].as_u64().unwrap(),
                    reason: "不能以重试重置预算".into()
                }
            )
            .is_err()
    );
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "blocked"
    );
    let mut f = Fixture::new(false);
    let (run, _, _) = f.running_executor();
    f.fix_run(&run);
    let d = fixture_delivery(&f, &run.worker_id, &run.delivery_id);
    assert!(
        f.store
            .execute(
                "execution-retry",
                &Command::MailboxRetry {
                    id: run.delivery_id.clone(),
                    revision: d["revision"].as_u64().unwrap(),
                    reason: "已受理代码执行只能新建有原因的返工".into()
                }
            )
            .is_err()
    );
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "blocked"
    );
}

#[test]
fn mailbox_retry_terminal_verification_only_returns_handled_and_preflight_keeps_run_budget() {
    let mut f = Fixture::new(false);
    let (run, _) = f.acceptance_fixture();
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture verifier stopped")
        .unwrap();
    let before = f.store.task(&run.task_id).unwrap();
    let d = fixture_delivery(&f, &run.worker_id, &run.delivery_id);
    let result = f
        .store
        .execute(
            "terminal-retry",
            &Command::MailboxRetry {
                id: run.delivery_id.clone(),
                revision: d["revision"].as_u64().unwrap(),
                reason: "只核对终局，不再运行".into(),
            },
        )
        .unwrap();
    assert_eq!(result["delivery"]["status"], "handled");
    assert_eq!(
        f.store.task(&run.task_id).unwrap().runs_used,
        before.runs_used
    );
    let mut f = Fixture::new(true);
    let config = f.prepare_digital_leader();
    let created = f.create();
    let id = created["delivery"]["deliveryId"].as_str().unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute(
        "UPDATE deliveries SET status='blocked',reason='fixture preflight failure' WHERE id=?1",
        [id],
    )
    .unwrap();
    let retry = Command::MailboxRetry {
        id: id.into(),
        revision: 1,
        reason: "配置条件已核对，重新评估".into(),
    };
    let queued = f.store.execute("retry", &retry).unwrap();
    assert_eq!(queued["delivery"]["status"], "queued");
    assert_eq!(f.store.execute("retry", &retry).unwrap(), queued);
    assert!(f.store.execute("retry-duplicate", &retry).is_err());
    let task = f
        .store
        .task(created["task"]["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(task.runs_used, 0);
    f.store.runtime_register("service", 123).unwrap();
    f.store.runtime_claim("service", id, &config).unwrap();
    assert_eq!(f.store.task(&task.id).unwrap().runs_used, 1);
}

#[test]
fn digital_leader_retry_uses_scoped_delivery_and_cannot_forge_retry_disposition() {
    let mut f = Fixture::new(true);
    f.prepare_executor();
    let (run, bound) = f.running_member_with_contract(Some(generic_contract()));
    assert_eq!(
        f.store
            .member_call(
                &bound,
                &member_operation(
                    &run,
                    "accept",
                    "task_intake",
                    json!({"revision":2,"decision":"accept","reason":"完整"})
                )
            )
            .unwrap()["ok"],
        true
    );
    let arranged = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &run,
                "arrange",
                "task_arrange",
                json!({"revision":3,"action":"execute","instruction":"执行"}),
            ),
        )
        .unwrap();
    assert_eq!(arranged["ok"], true);
    let id = arranged["data"]["delivery"]["deliveryId"].as_str().unwrap();
    let forged = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &run,
                "fake-retry",
                "message_respond",
                json!({"kind":"retry","deliveryId":id}),
            ),
        )
        .unwrap();
    assert_eq!(forged["ok"], false, "首次安排不能冒充重试");
    let description = f.store.member_description(&bound).unwrap();
    let tools = description["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "mailbox_retry"));
    let respond = tools
        .iter()
        .find(|t| t["name"] == "message_respond")
        .unwrap();
    assert!(respond["inputSchema"]["properties"]["deliveryId"].is_object());
    assert!(
        respond["inputSchema"]["properties"]["kind"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("retry"))
    );
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute(
        "UPDATE deliveries SET status='blocked',reason='fixture preflight failure' WHERE id=?1",
        [id],
    )
    .unwrap();
    let retry = member_operation(
        &run,
        "retry",
        "mailbox_retry",
        json!({"id":id,"revision":1,"reason":"原配置条件已核对"}),
    );
    let queued = f.store.member_call(&bound, &retry).unwrap();
    assert_eq!(queued["ok"], true, "{queued}");
    assert_eq!(f.store.member_call(&bound, &retry).unwrap(), queued);
    let read = f
        .store
        .member_call(
            &bound,
            &member_operation(&run, "read", "task_read", json!({})),
        )
        .unwrap();
    assert!(
        read["data"]["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == id && d["revision"] == 2)
    );
    let done = f
        .store
        .member_call(
            &bound,
            &member_operation(
                &run,
                "done",
                "message_respond",
                json!({"kind":"retry","deliveryId":id}),
            ),
        )
        .unwrap();
    assert_eq!(done["ok"], true, "{done}");
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture leader stopped")
        .unwrap();
    assert_eq!(f.store.task(&run.task_id).unwrap().reworks_used, 0);
}

#[test]
fn mailbox_retry_rechecks_receiver_permission_and_old_task_basis() {
    let mut f = Fixture::new(true);
    let (run, _) = f.running_member_with_contract(Some(generic_contract()));
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture stopped without terminal")
        .unwrap();
    choose_recovery_retry(&mut f, &run.task_id);
    let d = fixture_delivery(&f, &run.worker_id, &run.delivery_id);
    let command = Command::MailboxRetry {
        id: run.delivery_id.clone(),
        revision: d["revision"].as_u64().unwrap(),
        reason: "重新检查".into(),
    };
    let team = f.store.team(&f.team.id).unwrap();
    f.store
        .execute(
            "revoke-for-retry",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: team.id,
                revision: team.revision,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(run.worker_id.clone(), vec![Permission::Arrange])]),
            },
        )
        .unwrap();
    assert!(f.store.execute("revoked-retry", &command).is_err());
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "blocked"
    );
    let team = f.store.team(&f.team.id).unwrap();
    f.store
        .execute(
            "restore-for-retry",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: team.id,
                revision: team.revision,
                grant: BTreeMap::from([(run.worker_id.clone(), vec![Permission::Arrange])]),
                revoke: BTreeMap::new(),
            },
        )
        .unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute(
        "UPDATE tasks SET data=json_set(data,'$.revision',99) WHERE id=?1",
        [&run.task_id],
    )
    .unwrap();
    assert!(f.store.execute("stale-retry", &command).is_err());
    sql.execute(
        "UPDATE tasks SET data=json_set(data,'$.revision',?2) WHERE id=?1",
        rusqlite::params![run.task_id, run.task_revision],
    )
    .unwrap();
    assert_eq!(
        f.store.execute("restored-retry", &command).unwrap()["delivery"]["status"],
        "queued"
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "stopped");
}

fn choose_recovery_retry(f: &mut Fixture, task_id: &str) {
    let decisions = f.store.decisions(task_id).unwrap();
    for d in decisions
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"] == "recovery" && d["state"] == "open")
    {
        f.store
            .execute(
                &format!("respond-recovery-{}", d["id"].as_str().unwrap()),
                &Command::DecisionRespond {
                    id: d["id"].as_str().unwrap().into(),
                    revision: d["revision"].as_u64().unwrap(),
                    answer: "retry".into(),
                },
            )
            .unwrap();
    }
}

fn recovery_for(f: &Fixture, task: &str) -> DecisionRequest {
    let list = f.store.decisions(task).unwrap();
    serde_json::from_value(
        list.as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|d| d["kind"] == "recovery")
            .unwrap()
            .clone(),
    )
    .unwrap()
}
#[test]
fn recovery_wait_retry_and_cancel_are_explicit_and_never_release_unknown_resources() {
    let mut f = Fixture::new(true);
    let (run, _) = f.running_member_with_contract(Some(generic_contract()));
    f.store
        .runtime_run_unknown("service", &run.id, "fixture process identity unavailable")
        .unwrap();
    let d = recovery_for(&f, &run.task_id);
    assert_eq!(d.state, "open");
    assert_eq!(d.handler, f.human);
    assert!(
        f.store
            .execute(
                "premature-recovery",
                &Command::RecoveryApply {
                    id: d.id.clone(),
                    revision: 1
                }
            )
            .is_err()
    );
    let wait = acceptance_cli(
        &f,
        "choose-wait",
        &[
            "task",
            "decision",
            "respond",
            &d.id,
            "--revision",
            "1",
            "--answer",
            "wait",
        ],
    );
    assert_eq!(wait["data"]["decision"]["state"], "responded");
    let waiting = f
        .store
        .execute(
            "apply-wait",
            &Command::RecoveryApply {
                id: d.id.clone(),
                revision: 2,
            },
        )
        .unwrap();
    assert_eq!(waiting["applied"], false);
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    acceptance_cli(
        &f,
        "choose-retry",
        &[
            "task",
            "decision",
            "respond",
            &d.id,
            "--revision",
            "2",
            "--answer",
            "retry",
        ],
    );
    let denied = acceptance_cli(
        &f,
        "apply-before-stop",
        &["task", "recovery", "apply", &d.id, "--revision", "3"],
    );
    assert_eq!(denied["data"]["applied"], false);
    assert!(
        denied["data"]["decision"]["blocked_reason"]
            .as_str()
            .unwrap()
            .contains("停止")
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    f.store
        .runtime_run_observed_stopped(
            "service",
            &run.id,
            "fixture trusted resource inspection confirmed stop",
        )
        .unwrap();
    assert_eq!(
        f.store
            .decisions(&run.task_id)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["kind"] == "recovery")
            .count(),
        1
    );
    let resumed = acceptance_cli(
        &f,
        "apply-retry",
        &["task", "recovery", "apply", &d.id, "--revision", "4"],
    );
    assert_eq!(resumed["data"]["applied"], true);
    assert_eq!(resumed["data"]["decision"]["state"], "resolved");
    assert_eq!(resumed["data"]["result"]["delivery"]["status"], "queued");
    let again = acceptance_cli(
        &f,
        "apply-retry",
        &["task", "recovery", "apply", &d.id, "--revision", "4"],
    );
    assert_eq!(again, resumed);
    let next = f
        .store
        .runtime_claim("service", &run.delivery_id, &run.configuration_id)
        .unwrap();
    f.store.runtime_begin_launch("service", &next.id).unwrap();
    f.store
        .runtime_run_unknown("service", &next.id, "fixture second interruption")
        .unwrap();
    let newer = recovery_for(&f, &run.task_id);
    assert_ne!(newer.id, d.id);
    assert_eq!(f.store.decision(&d.id).unwrap().state, "resolved");
    f.store
        .execute(
            "choose-cancel",
            &Command::DecisionRespond {
                id: newer.id.clone(),
                revision: 1,
                answer: "cancel".into(),
            },
        )
        .unwrap();
    assert!(!f.store.task(&run.task_id).unwrap().cancellation_requested);
    let cancelled = f
        .store
        .execute(
            "apply-cancel",
            &Command::RecoveryApply {
                id: newer.id.clone(),
                revision: 2,
            },
        )
        .unwrap();
    assert_eq!(cancelled["decision"]["state"], "resolved");
    assert!(f.store.task(&run.task_id).unwrap().cancellation_requested);
    assert_ne!(f.store.task(&run.task_id).unwrap().state, "closed");
    assert_eq!(f.store.run(&next.id).unwrap().state, "unknown");
    f.store
        .runtime_run_observed_stopped("service", &next.id, "fixture all resources finally stopped")
        .unwrap();
    assert_eq!(
        f.store.task(&run.task_id).unwrap().outcome.as_deref(),
        Some("cancelled")
    );
}

#[test]
fn recovery_response_and_retry_roll_back_with_their_receipts_and_request_ledger() {
    let mut f = Fixture::new(true);
    let (run, _) = f.running_member_with_contract(Some(generic_contract()));
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture stopped unfinished")
        .unwrap();
    let d = recovery_for(&f, &run.task_id);
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_recovery_response BEFORE INSERT ON messages WHEN NEW.kind='decision.result' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let respond = Command::DecisionRespond {
        id: d.id.clone(),
        revision: 1,
        answer: "retry".into(),
    };
    assert!(f.store.execute("choose", &respond).is_err());
    assert_eq!(f.store.decision(&d.id).unwrap().state, "open");
    assert_eq!(
        fixture_delivery(&f, &f.human, &d.request_delivery)["status"],
        "queued"
    );
    sql.execute_batch("DROP TRIGGER fail_recovery_response;")
        .unwrap();
    f.store.execute("choose", &respond).unwrap();
    let apply = Command::RecoveryApply {
        id: d.id.clone(),
        revision: 2,
    };
    sql.execute_batch("CREATE TRIGGER fail_recovery_ledger BEFORE INSERT ON requests WHEN NEW.id='apply' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(f.store.execute("apply", &apply).is_err());
    assert_eq!(f.store.decision(&d.id).unwrap().state, "responded");
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "blocked"
    );
    sql.execute_batch("DROP TRIGGER fail_recovery_ledger;")
        .unwrap();
    let applied = f.store.execute("apply", &apply).unwrap();
    assert_eq!(applied["applied"], true);
    assert_eq!(f.store.decision(&d.id).unwrap().state, "resolved");
}

#[test]
fn revoked_leader_gets_one_budget_exempt_recovery_and_human_response_does_not_grant_permission() {
    let mut f = Fixture::new(true);
    f.prepare_digital_leader();
    let task = f.create();
    let id = task["task"]["id"].as_str().unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute("UPDATE tasks SET data=json_set(data,'$.messages_used',json_extract(data,'$.contract.max_messages')) WHERE id=?1",[id]).unwrap();
    let before = f.store.task(id).unwrap().messages_used;
    let team = f.store.team(&f.team.id).unwrap();
    f.store
        .execute(
            "revoke-leader",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: team.id,
                revision: team.revision,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(f.team.leader.clone(), vec![Permission::Arrange])]),
            },
        )
        .unwrap();
    let d = recovery_for(&f, id);
    assert_eq!(d.state, "open");
    assert_eq!(f.store.task(id).unwrap().messages_used, before);
    let original = d.recovery.as_ref().unwrap().delivery_id.clone();
    let receipt = fixture_delivery(&f, &f.team.leader, &original);
    assert!(
        f.store
            .execute(
                "no-choice-retry",
                &Command::MailboxRetry {
                    id: original,
                    revision: receipt["revision"].as_u64().unwrap(),
                    reason: "必须先由本人选择".into()
                }
            )
            .is_err()
    );
    f.store
        .execute(
            "choose-retry",
            &Command::DecisionRespond {
                id: d.id.clone(),
                revision: 1,
                answer: "retry".into(),
            },
        )
        .unwrap();
    let failed = f
        .store
        .execute(
            "apply-denied",
            &Command::RecoveryApply {
                id: d.id.clone(),
                revision: 2,
            },
        )
        .unwrap();
    assert_eq!(failed["applied"], false);
    assert!(
        failed["decision"]["blocked_reason"]
            .as_str()
            .unwrap()
            .contains("授权")
    );
    assert!(
        !f.store.team(&f.team.id).unwrap().grants[&f.team.leader].contains(&Permission::Arrange)
    );
    assert_eq!(f.store.task(id).unwrap().runs_used, 0);
    let team = f.store.team(&f.team.id).unwrap();
    f.store
        .execute(
            "restore-leader",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: team.id,
                revision: team.revision,
                grant: BTreeMap::from([(f.team.leader.clone(), vec![Permission::Arrange])]),
                revoke: BTreeMap::new(),
            },
        )
        .unwrap();
    let applied = f
        .store
        .execute(
            "apply-restored",
            &Command::RecoveryApply {
                id: d.id.clone(),
                revision: 3,
            },
        )
        .unwrap();
    assert_eq!(applied["applied"], true);
    assert_eq!(f.store.task(id).unwrap().messages_used, before);
    assert_eq!(f.store.task(id).unwrap().runs_used, 0);
}

#[cfg(unix)]
#[test]
fn runtime_reconcile_requires_real_process_absence_and_never_dispatches() {
    use std::os::unix::process::CommandExt;
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = ChildGuard(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let mut f = Fixture::new(true);
    let (run, _) = f.running_member_with_contract(Some(generic_contract()));
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    // The fixture supplies launch registration, but resource observations use
    // the real OS process table through the public CLI, never a stopped flag.
    sql.execute(
        "UPDATE runs SET data=json_set(data,'$.pid',NULL) WHERE id=?1",
        [&run.id],
    )
    .unwrap();
    let first = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(first["data"]["state"], "blocked_unknown");
    sql.execute(
        "INSERT INTO api_launches(run_id,data) VALUES(?1,'{}')",
        [&run.id],
    )
    .unwrap();
    let missing = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(
        missing["data"]["state"], "blocked_unknown",
        "启动登记窗没有 PID，不能猜测已停止"
    );
    sql.execute(
        "UPDATE runs SET data=json_set(data,'$.pid',?2) WHERE id=?1",
        rusqlite::params![run.id, child.0.id()],
    )
    .unwrap();
    let alive = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(alive["data"]["state"], "blocked_unknown");
    assert_eq!(alive["data"]["unresolvedRunIds"], json!([run.id]));
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "核对不能按旧 PID 杀进程"
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let stopped = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(stopped["data"]["state"], "stopped", "{stopped}");
    assert_eq!(f.store.run(&run.id).unwrap().state, "stopped");
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "blocked"
    );
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
    assert_eq!(recovery_for(&f, &run.task_id).state, "open");
    let repeated = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(repeated["data"]["activeRuns"], 0);
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
    assert_eq!(
        f.store
            .decisions(&run.task_id)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn runtime_reconcile_refuses_service_lock_and_leaves_queued_work_untouched() {
    use fs2::FileExt;
    let mut f = Fixture::new(true);
    let created = f.create();
    let task = created["task"]["id"].as_str().unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.dir.path().join("runtime.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let before = f.store.runtime_snapshot().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(f.dir.path())
        .args(["--json", "runtime", "reconcile"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let denied: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(denied["ok"], false);
    assert_eq!(denied["error"]["code"], "conflict");
    assert_eq!(f.store.runtime_snapshot().unwrap(), before);
    drop(lock);
    let checked = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(checked["data"]["state"], "stopped");
    assert_eq!(
        checked["data"]["queuedDeliveries"],
        before["queuedDeliveries"]
    );
    assert_eq!(f.store.task(task).unwrap().runs_used, 0);
    assert_eq!(
        f.store.mailbox(Some(&f.team.leader)).unwrap()[0]["status"],
        "queued"
    );
}

#[test]
fn recovery_retry_closes_receipt_when_original_terminal_is_already_durable() {
    let mut f = Fixture::new(true);
    let (run, binding) = f.running_member_with_contract(Some(generic_contract()));
    let response = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &run,
                "wait-terminal",
                "message_respond",
                json!({"kind":"wait","handler":f.human,"reason":"已保存的处理终局"}),
            ),
        )
        .unwrap();
    assert_eq!(response["ok"], true, "{response}");
    f.store
        .runtime_run_unknown("service", &run.id, "fixture lost terminal transport")
        .unwrap();
    choose_recovery_retry(&mut f, &run.task_id);
    let d = recovery_for(&f, &run.task_id);
    f.store
        .runtime_run_observed_stopped("service", &run.id, "fixture trusted stop observation")
        .unwrap();
    let receipt = fixture_delivery(&f, &run.worker_id, &run.delivery_id);
    assert_eq!(receipt["status"], "handled");
    let result = acceptance_cli(
        &f,
        "retry-finished",
        &[
            "mailbox",
            "retry",
            &run.delivery_id,
            "--revision",
            &receipt["revision"].to_string(),
            "--reason",
            "仅核对已提交终局",
        ],
    );
    assert_eq!(result["data"]["delivery"]["status"], "handled");
    assert_eq!(f.store.decision(&d.id).unwrap().state, "resolved");
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
}

#[test]
fn host_skill_uses_actual_frozen_role_and_current_permissions() {
    let mut f = Fixture::new(true);
    let created = f.create();
    let id = created["task"]["id"].as_str().unwrap();
    let describe = |f: &Fixture| {
        acceptance_cli(
            f,
            "unused",
            &["skill", "describe", "--protocol", "2", "--task", id],
        )["data"]
            .clone()
    };
    let initial = describe(&f);
    let has = |v: &Value, id: &str| {
        v["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == id)
    };
    assert_eq!(initial["isTeamLeader"], false);
    assert!(
        !has(&initial, "coordination"),
        "管理者不能借 Skill 冒充数字负责人"
    );
    assert!(has(&initial, "recovery"));
    assert!(has(&initial, "acceptance"));
    f.store
        .execute(
            "skill-revoke",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 1,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(f.human.clone(), vec![Permission::Accept])]),
            },
        )
        .unwrap();
    let revoked = describe(&f);
    assert!(!has(&revoked, "acceptance"));
    assert_eq!(revoked["scope"]["authorizationRevision"], 2);
    assert!(has(&revoked, "recovery"));
    let human_leader = f.human.clone();
    let mut patch = f.team.clone();
    patch.leader = human_leader;
    // A newer team role does not rewrite the existing task snapshot.
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute(
        "UPDATE teams SET data=json_set(data,'$.leader',?2) WHERE id=?1",
        rusqlite::params![f.team.id, patch.leader],
    )
    .unwrap();
    assert!(!has(&describe(&f), "coordination"));
    let mut human = Fixture::new(false);
    let created = human.create();
    let value = human
        .store
        .host_skill_description(None, created["task"]["id"].as_str())
        .unwrap();
    assert!(has(&value, "coordination"));
    assert_eq!(value["isTeamLeader"], true);
}

#[test]
fn member_skill_bundle_contains_exact_installed_operations_and_only_relevant_guidance() {
    let check = |description: &Value| {
        let tools = description["tools"].as_array().unwrap();
        let files = description["files"].as_object().unwrap();
        let operations: Vec<_> = files
            .keys()
            .filter(|p| p.starts_with("operations/"))
            .collect();
        assert_eq!(operations.len(), tools.len());
        for tool in tools {
            let path = format!("operations/{}.md", tool["name"].as_str().unwrap());
            let doc = files[&path].as_str().unwrap();
            let schema = doc
                .split("```json\n")
                .nth(1)
                .unwrap()
                .split("\n```")
                .next()
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(schema).unwrap(),
                tool["inputSchema"]
            );
        }
        assert_eq!(
            description["skillDigest"],
            atelier::content::digest(description["skill"].as_str().unwrap().as_bytes())
        );
        assert!(!files.contains_key("references/host.md"));
    };
    let mut coordinator = Fixture::new(true);
    let (_, binding) = coordinator.running_member_with_contract(Some(generic_contract()));
    let d = coordinator.store.member_description(&binding).unwrap();
    check(&d);
    assert!(d["files"].get("references/coordination.md").is_some());
    assert!(d["files"].get("references/execution.md").is_none());
    assert!(d["files"].get("references/verification.md").is_none());
    let mut executor = Fixture::new(false);
    let (_, binding, _) = executor.running_executor();
    let d = executor.store.member_description(&binding).unwrap();
    check(&d);
    assert!(d["files"].get("references/execution.md").is_some());
    assert!(d["files"].get("references/coordination.md").is_none());
    assert!(d["files"].get("operations/acceptance_request.md").is_none());
    let mut verifier = Fixture::new(false);
    let (_, binding) =
        verifier.code_artifact_for_check("<h1>fixture</h1>", &format!("sha256:{}", "a".repeat(64)));
    let d = verifier.store.member_description(&binding).unwrap();
    check(&d);
    assert!(d["files"].get("references/verification.md").is_some());
    assert!(d["files"].get("references/execution.md").is_none());
    assert!(d["files"].get("operations/write_file.md").is_none());
}

const CLI_RESOURCE_IMAGE: &str =
    "sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133";
fn prepared_cli_fixture() -> (Fixture, Run) {
    prepared_cli_fixture_with_policy(Some(vec!["example.com".into()]))
}
fn prepared_cli_fixture_with_policy(egress_hosts: Option<Vec<String>>) -> (Fixture, Run) {
    prepared_cli_fixture_with_route(egress_hosts, None)
}
fn prepared_cli_fixture_with_route(
    egress_hosts: Option<Vec<String>>,
    egress_proxy: Option<atelier::connection::EgressProxy>,
) -> (Fixture, Run) {
    let mut f = Fixture::new(true);
    f.prepare_digital_leader();
    let connection = f
        .store
        .execute(
            "cli-connection",
            &Command::ConnectionCreate {
                name: "CLI 资源夹具；不运行模型".into(),
                specification: atelier::connection::ConnectionSpec::AgentCli {
                    runtime: "pi".into(),
                    model: None,
                    image: Some(CLI_RESOURCE_IMAGE.into()),
                    egress_hosts,
                    egress_proxy,
                },
            },
        )
        .unwrap();
    let member = f
        .store
        .execute(
            "cli-worker",
            &Command::WorkerUpdate {
                id: f.team.leader.clone(),
                revision: 2,
                name: None,
                description: None,
                connection: Some(connection["connection"]["id"].as_str().unwrap().into()),
                clear_connection: false,
            },
        )
        .unwrap();
    let task = f.create();
    f.store.runtime_register("service", 123).unwrap();
    let run = f
        .store
        .runtime_claim(
            "service",
            task["delivery"]["deliveryId"].as_str().unwrap(),
            member["execution_config"].as_str().unwrap(),
        )
        .unwrap();
    (f, run)
}

#[test]
fn cli_egress_policy_is_explicit_versioned_and_cannot_expand_an_existing_run() {
    use atelier::connection::ConnectionSpec;
    let old = json!({"transport":"agent-cli","runtime":"pi","model":null,"image":null});
    let spec: ConnectionSpec = serde_json::from_value(old.clone()).unwrap();
    spec.validate().unwrap();
    assert_eq!(
        serde_json::to_value(spec).unwrap(),
        old,
        "old versions keep their serialized fingerprint"
    );
    for hosts in [
        json!([]),
        json!(["*"]),
        json!(["*.example.com"]),
        json!(["127.0.0.1"]),
        json!(["https://example.com"]),
        json!(["EXAMPLE.com"]),
        json!(["example.com."]),
        json!(["example.com:443"]),
        json!(["example.com", "example.com"]),
        json!(["-bad.example"]),
        json!(["bad-.example"]),
        json!(["a..example"]),
        json!(["localhost"]),
    ] {
        let mut value = old.clone();
        value["egress_hosts"] = hosts;
        assert!(
            serde_json::from_value::<ConnectionSpec>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let (mut f, run) = prepared_cli_fixture();
    let configuration = f
        .store
        .execution_configuration(&run.configuration_id)
        .unwrap();
    let version = f
        .store
        .connection_version(&configuration.connection_version)
        .unwrap();
    let updated = f
        .store
        .execute(
            "expand-cli-policy",
            &Command::ConnectionUpdate {
                id: version.connection_id.clone(),
                revision: 1,
                name: None,
                specification: Some(ConnectionSpec::AgentCli {
                    runtime: "pi".into(),
                    model: None,
                    image: Some(CLI_RESOURCE_IMAGE.into()),
                    egress_hosts: Some(vec!["example.com".into(), "another.example".into()]),
                    egress_proxy: None,
                }),
            },
        )
        .unwrap();
    assert_ne!(updated["version"]["id"], version.id);
    let context = f
        .store
        .runtime_context(
            "service",
            &run.task_id,
            &run.worker_id,
            &run.configuration_id,
            &run.purpose,
        )
        .unwrap();
    let record = f
        .store
        .runtime_begin_cli_launch("service", &run.id, "fixture-engine", &context)
        .unwrap();
    assert_eq!(
        record.egress_hosts,
        vec!["example.com"],
        "launch uses the run's frozen connection, not current settings"
    );

    let (mut missing, run) = prepared_cli_fixture_with_policy(None);
    let context = missing
        .store
        .runtime_context(
            "service",
            &run.task_id,
            &run.worker_id,
            &run.configuration_id,
            &run.purpose,
        )
        .unwrap();
    assert!(
        missing
            .store
            .runtime_begin_cli_launch("service", &run.id, "fixture-engine", &context)
            .unwrap_err()
            .to_string()
            .contains("出站策略")
    );
    assert!(!missing.store.run(&run.id).unwrap().launch_started);
    assert!(missing.store.cli_resources(&run.id).unwrap().is_none());
    assert!(
        !missing
            .store
            .runtime_context(
                "service",
                &run.task_id,
                &run.worker_id,
                &run.configuration_id,
                &run.purpose
            )
            .unwrap()
            .used
    );
}

#[test]
fn cli_resources_registration_is_atomic_bound_to_frozen_configuration_and_gates_stop() {
    let mut api = Fixture::new(true);
    let config = api.prepare_digital_leader();
    let task = api.create();
    api.store.runtime_register("service", 123).unwrap();
    let run = api
        .store
        .runtime_claim(
            "service",
            task["delivery"]["deliveryId"].as_str().unwrap(),
            &config,
        )
        .unwrap();
    assert!(
        api.store
            .runtime_begin_cli_resources("service", &run.id, "fixture-engine")
            .is_err()
    );
    assert!(!api.store.run(&run.id).unwrap().launch_started);

    let (mut f, run) = prepared_cli_fixture();
    assert!(
        f.store
            .runtime_begin_cli_resources("obsolete", &run.id, "fixture-engine")
            .is_err()
    );
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_cli_launch BEFORE UPDATE ON runs BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.store
            .runtime_begin_cli_resources("service", &run.id, "fixture-engine")
            .is_err()
    );
    assert!(f.store.cli_resources(&run.id).unwrap().is_none());
    assert!(!f.store.run(&run.id).unwrap().launch_started);
    sql.execute_batch("DROP TRIGGER reject_cli_launch;")
        .unwrap();
    let record = f
        .store
        .runtime_begin_cli_resources("service", &run.id, "fixture-engine")
        .unwrap();
    assert_eq!(record.image, CLI_RESOURCE_IMAGE);
    assert_eq!(record.egress_hosts, vec!["example.com"]);
    assert_eq!(record.run_id, run.id);
    assert_eq!(
        record.labels()["atelier.workspace"],
        f.store.workspace().unwrap()["id"]
    );
    assert!(f.store.run(&run.id).unwrap().launch_started);
    assert!(
        f.store
            .runtime_begin_cli_resources("service", &run.id, "fixture-engine")
            .is_err()
    );
    assert!(
        f.store
            .runtime_run_observed_stopped("service", &run.id, "fixture premature stop")
            .is_err()
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "prepared");
    assert_eq!(
        Store::open(f.dir.path())
            .unwrap()
            .cli_resources(&run.id)
            .unwrap()
            .unwrap()
            .ownership_token,
        record.ownership_token
    );
    let shown = acceptance_cli(&f, "unused", &["run", "show", &run.id]);
    assert_eq!(shown["data"]["cliResources"]["resourcesStopped"], false);
    assert!(shown["data"]["apiExecution"].is_null());
}

#[test]
fn cli_launch_context_use_and_resource_intent_commit_together_and_missing_history_blocks() {
    use std::fs;
    let (mut f, run) = prepared_cli_fixture();
    let context = f
        .store
        .runtime_context(
            "service",
            &run.task_id,
            &run.worker_id,
            &run.configuration_id,
            &run.purpose,
        )
        .unwrap();
    context.prepare_cli_directories(f.dir.path()).unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_cli_launch BEFORE INSERT ON cli_resources BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.store
            .runtime_begin_cli_launch("service", &run.id, "fixture-engine", &context)
            .is_err()
    );
    assert!(!f.store.run(&run.id).unwrap().launch_started);
    assert!(f.store.cli_resources(&run.id).unwrap().is_none());
    assert!(
        !f.store
            .runtime_context(
                "service",
                &run.task_id,
                &run.worker_id,
                &run.configuration_id,
                &run.purpose
            )
            .unwrap()
            .used
    );
    sql.execute_batch("DROP TRIGGER reject_cli_launch").unwrap();
    let mut foreign = context.clone();
    foreign.id = "foreign-context".into();
    assert!(
        f.store
            .runtime_begin_cli_launch("service", &run.id, "fixture-engine", &foreign)
            .is_err()
    );
    let record = f
        .store
        .runtime_begin_cli_launch("service", &run.id, "fixture-engine", &context)
        .unwrap();
    assert_eq!(record.context_id.as_deref(), Some(context.id.as_str()));
    assert_eq!(record.resume, Some(false));
    drop(f.store);
    f.store = Store::open(f.dir.path()).unwrap();
    let used = f
        .store
        .runtime_context(
            "service",
            &run.task_id,
            &run.worker_id,
            &run.configuration_id,
            &run.purpose,
        )
        .unwrap();
    assert!(used.used);
    assert!(
        used.prepare_cli_directories(f.dir.path()).is_err(),
        "committed launch cannot silently recreate absent native history"
    );
    let (native, _) = used.directories(f.dir.path());
    fs::write(
        native.join("binding.json"),
        "fixture binding; adapter validates its content",
    )
    .unwrap();
    for name in ["sdk", "sessions", "journal", "cwd"] {
        fs::create_dir(native.join(name)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(native.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    used.prepare_cli_directories(f.dir.path()).unwrap();
    assert!(
        used.prepare_directories(f.dir.path()).is_err(),
        "CLI layout cannot satisfy the API checkpoint contract"
    );
    fs::remove_dir(native.join("sessions")).unwrap();
    assert!(used.prepare_cli_directories(f.dir.path()).is_err());
    assert!(!native.join("sessions").exists());
}

#[cfg(unix)]
#[test]
fn cli_reconcile_missing_adapter_pid_keeps_resources_unknown_without_docker_or_new_runs() {
    let (mut f, run) = prepared_cli_fixture();
    f.store
        .runtime_begin_cli_resources("service", &run.id, "fixture-engine")
        .unwrap();
    let result = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(result["data"]["state"], "blocked_unknown");
    assert_eq!(result["data"]["reconciledCliResources"], 0);
    let record = f.store.cli_resources(&run.id).unwrap().unwrap();
    assert!(!record.resources_stopped);
    assert!(record.diagnostic.unwrap().contains("缺少启动登记"));
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
    assert_eq!(f.store.run(&run.id).unwrap().state, "unknown");
    let epoch = result["data"]["epoch"].as_str().unwrap();
    assert!(
        f.store
            .runtime_run_observed_stopped(epoch, &run.id, "cannot bypass")
            .is_err()
    );
}

#[cfg(unix)]
#[test]
#[ignore = "需要真实 Docker 和预先准备的固定 Node 镜像；不运行真实 agent CLI"]
fn real_docker_cli_reconcile_preserves_foreign_resources_and_waits_for_adapter_exit() {
    use std::os::unix::process::CommandExt;
    struct Resources {
        child: std::process::Child,
        containers: Vec<String>,
        networks: Vec<String>,
    }
    impl Drop for Resources {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            for id in self.containers.iter().rev() {
                let _ = std::process::Command::new("docker")
                    .args(["container", "rm", "-f", id])
                    .output();
            }
            for id in self.networks.iter().rev() {
                let _ = std::process::Command::new("docker")
                    .args(["network", "rm", id])
                    .output();
            }
        }
    }
    fn docker(args: &[&str]) -> String {
        let output = std::process::Command::new("docker")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "docker {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
    let mut resources = Resources {
        child: std::process::Command::new("/bin/sleep")
            .arg("120")
            .process_group(0)
            .spawn()
            .unwrap(),
        containers: vec![],
        networks: vec![],
    };
    let engine_id = docker(&["info", "--format", "{{.ID}}"]);
    let (mut f, run) = prepared_cli_fixture();
    let record = f
        .store
        .runtime_begin_cli_resources("service", &run.id, &engine_id)
        .unwrap();
    f.store
        .runtime_child_started(
            "service",
            &run.id,
            resources.child.id(),
            "actual test child in private process group",
        )
        .unwrap();
    let labels = record
        .labels()
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>();
    let label_args = labels
        .iter()
        .flat_map(|v| ["--label", v.as_str()])
        .collect::<Vec<_>>();
    for name in [&record.internal_network, &record.egress_network] {
        let mut args = vec![
            "network",
            "create",
            "--internal",
            "--opt",
            "com.docker.network.bridge.inhibit_ipv4=true",
        ];
        args.extend(&label_args);
        args.push(name);
        resources.networks.push(docker(&args));
    }
    let make_container = |name: &str, owner_labels: &[&str]| {
        let mut args = vec![
            "create",
            "--name",
            name,
            "--network",
            &record.internal_network,
            "--pull=never",
            "--read-only",
            "--user",
            "65534:65534",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--memory",
            "128m",
            "--cpus",
            "1",
            "--pids-limit",
            "32",
        ];
        args.extend(owner_labels);
        args.extend([
            "--entrypoint",
            "node",
            CLI_RESOURCE_IMAGE,
            "-e",
            "setInterval(()=>{},1000)",
        ]);
        let id = docker(&args);
        docker(&["start", &id]);
        id
    };
    let execution = make_container(&record.execution_container, &label_args);
    resources.containers.push(execution.clone());
    // Same name and run/workspace labels but a different ownership token.
    let wrong_labels = labels
        .iter()
        .map(|l| {
            if l.starts_with("atelier.owner=") {
                "atelier.owner=foreign-fixture".into()
            } else {
                l.clone()
            }
        })
        .collect::<Vec<String>>();
    let wrong_args = wrong_labels
        .iter()
        .flat_map(|v| ["--label", v.as_str()])
        .collect::<Vec<_>>();
    let foreign = make_container(&record.proxy_container, &wrong_args);
    resources.containers.push(foreign.clone());
    let alive = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(alive["data"]["state"], "blocked_unknown");
    assert_eq!(
        docker(&["inspect", "--format", "{{.State.Running}}", &execution]),
        "true"
    );
    assert!(resources.child.try_wait().unwrap().is_none());
    resources.child.kill().unwrap();
    resources.child.wait().unwrap();
    // Wrong-daemon observation must not delete anything or release the Run.
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    sql.execute("UPDATE cli_resources SET data=json_set(data,'$.engineId','another-engine') WHERE run_id=?1", [&run.id]).unwrap();
    let wrong_engine = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(wrong_engine["data"]["state"], "blocked_unknown");
    assert!(
        f.store
            .cli_resources(&run.id)
            .unwrap()
            .unwrap()
            .diagnostic
            .unwrap()
            .contains("引擎已变化")
    );
    assert_eq!(
        docker(&["inspect", "--format", "{{.State.Running}}", &execution]),
        "true"
    );
    sql.execute(
        "UPDATE cli_resources SET data=json_set(data,'$.engineId',?2) WHERE run_id=?1",
        rusqlite::params![run.id, engine_id],
    )
    .unwrap();
    let foreign_result = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(foreign_result["data"]["state"], "blocked_unknown");
    assert!(
        f.store
            .cli_resources(&run.id)
            .unwrap()
            .unwrap()
            .diagnostic
            .unwrap()
            .contains("归属不符")
    );
    assert_eq!(
        docker(&["inspect", "--format", "{{.State.Running}}", &foreign]),
        "true"
    );
    assert_eq!(
        docker(&[
            "container",
            "ls",
            "-a",
            "--filter",
            &format!("id={execution}"),
            "--format",
            "{{.ID}}"
        ]),
        ""
    );
    docker(&["container", "rm", "-f", &foreign]);
    resources
        .containers
        .push(make_container(&record.proxy_container, &label_args));
    let outsider = make_container(
        &format!("atelier-outsider-{}", record.ownership_token),
        &wrong_args,
    );
    resources.containers.push(outsider.clone());
    let attached = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(attached["data"]["state"], "blocked_unknown");
    assert!(
        f.store
            .cli_resources(&run.id)
            .unwrap()
            .unwrap()
            .diagnostic
            .unwrap()
            .contains("挂接资源")
    );
    assert_eq!(
        docker(&["inspect", "--format", "{{.State.Running}}", &outsider]),
        "true"
    );
    docker(&["container", "rm", "-f", &outsider]);
    let final_result = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(final_result["data"]["state"], "stopped", "{final_result}");
    assert_eq!(final_result["data"]["reconciledCliResources"], 1);
    assert!(
        f.store
            .cli_resources(&run.id)
            .unwrap()
            .unwrap()
            .resources_stopped
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "stopped");
    assert_eq!(
        fixture_delivery(&f, &run.worker_id, &run.delivery_id)["status"],
        "blocked"
    );
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
    assert_eq!(recovery_for(&f, &run.task_id).state, "open");
    let repeated = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(repeated["data"]["reconciledCliResources"], 0);
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
    for name in [&record.internal_network, &record.egress_network] {
        assert_eq!(
            docker(&[
                "network",
                "ls",
                "--filter",
                &format!("name={name}"),
                "--format",
                "{{.Name}}"
            ]),
            ""
        );
    }
    let evidence = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        ".agents/verify-runs/1/cli-recovery-{}.json",
        record.ownership_token
    ));
    std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    std::fs::write(&evidence,serde_json::to_vec_pretty(&json!({"kind":"development_integration","cliExecution":false,"image":CLI_RESOURCE_IMAGE,
        "liveAdapter":alive["data"],"wrongEngine":wrong_engine["data"],"foreignResource":foreign_result["data"],"attachedNetwork":attached["data"],"stopped":final_result["data"],"repeat":repeated["data"]})).unwrap()).unwrap();
    println!("evidence: {}", evidence.display());
}

#[test]
fn cli_production_launch_binds_login_generation_and_ungranted_crash_releases_no_containers() {
    let (mut f, run) = prepared_cli_fixture();
    let context = f
        .store
        .runtime_context(
            "service",
            &run.task_id,
            &run.worker_id,
            &run.configuration_id,
            &run.purpose,
        )
        .unwrap();
    let config = f
        .store
        .execution_configuration(&run.configuration_id)
        .unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    let environment = json!({"id":run.configuration_id,"workerId":run.worker_id,"connectionVersion":config.connection_version,
        "runtime":"pi","image":CLI_RESOURCE_IMAGE,"engineId":"fixture-engine","state":"prepared","code":"fixture",
        "loginGeneration":4,"loginMaterialReady":true,"everPrepared":true,"preparationRequestId":"fixture"});
    sql.execute(
        "INSERT INTO cli_environments(id,data) VALUES(?1,?2)",
        rusqlite::params![run.configuration_id, environment.to_string()],
    )
    .unwrap();
    for (engine, generation) in [("fixture-engine", 3), ("other-engine", 4)] {
        assert!(
            f.store
                .runtime_begin_cli_execution("service", &run.id, engine, &context, generation)
                .is_err()
        );
        assert!(!f.store.run(&run.id).unwrap().launch_started);
        assert!(
            !f.store
                .runtime_context(
                    "service",
                    &run.task_id,
                    &run.worker_id,
                    &run.configuration_id,
                    &run.purpose
                )
                .unwrap()
                .used
        );
    }
    sql.execute_batch("CREATE TRIGGER reject_cli_permission BEFORE INSERT ON cli_resources BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        f.store
            .runtime_begin_cli_execution("service", &run.id, "fixture-engine", &context, 4)
            .is_err()
    );
    assert!(!f.store.run(&run.id).unwrap().launch_started);
    sql.execute_batch("DROP TRIGGER reject_cli_permission")
        .unwrap();
    let resource = f
        .store
        .runtime_begin_cli_execution("service", &run.id, "fixture-engine", &context, 4)
        .unwrap();
    assert_eq!(resource.login_generation, Some(4));
    assert_eq!(resource.creation_authorized, Some(false));
    assert!(
        f.store
            .runtime_authorize_cli_creation("service", &run.id)
            .is_err(),
        "no registered child, no creation permit"
    );
    // No Docker exists at this point, so reconciliation must not require one.
    let result = acceptance_cli(&f, "unused", &["runtime", "reconcile"]);
    assert_eq!(result["data"]["state"], "stopped");
    assert_eq!(result["data"]["reconciledCliResources"], 1);
    assert!(
        f.store
            .cli_resources(&run.id)
            .unwrap()
            .unwrap()
            .resources_stopped
    );
    assert_eq!(f.store.run(&run.id).unwrap().state, "stopped");
    assert_eq!(f.store.task(&run.task_id).unwrap().runs_used, 1);
    assert!(
        f.store
            .runtime_authorize_cli_creation("service", &run.id)
            .is_err()
    );
}

#[test]
fn foreign_delivery_reconciliation_is_read_only_context_bound_and_reauthorizes() {
    use atelier::member::ReconcileOperation;
    let mut f = Fixture::new(true);
    let (old, binding) = f.running_member();
    let send = member_operation(
        &old,
        "committed",
        "message_send",
        json!({"recipient":f.human,"kind":"work.note","body":"只发送一次"}),
    );
    let saved = f.store.member_call(&binding, &send).unwrap();
    let next_message = f
        .store
        .member_call(
            &binding,
            &member_operation(
                &old,
                "self-message",
                "message_send",
                json!({"recipient":old.worker_id,"kind":"work.note","body":"后续投递"}),
            ),
        )
        .unwrap();
    let next_delivery = next_message["data"]["deliveryId"]
        .as_str()
        .unwrap()
        .to_string();
    f.store
        .member_call(
            &binding,
            &member_operation(
                &old,
                "wait",
                "message_respond",
                json!({"kind":"wait","reason":"fixture","handler":f.human}),
            ),
        )
        .unwrap();
    f.store
        .runtime_run_observed_stopped("service", &old.id, "fixture stopped")
        .unwrap();
    let next = f
        .store
        .runtime_claim("service", &next_delivery, &old.configuration_id)
        .unwrap();
    f.store.runtime_begin_launch("service", &next.id).unwrap();
    f.store
        .runtime_child_started("service", &next.id, 322, "fixture next")
        .unwrap();
    let current = f.store.bind_member("service", &next.id).unwrap();
    let sql = rusqlite::Connection::open(f.dir.path().join("atelier.sqlite3")).unwrap();
    // Explicit context metadata; no credential/model process in this core test.
    for run in [&old.id, &next.id] {
        sql.execute(
            "INSERT INTO api_launches(run_id,data) VALUES(?1,?2)",
            rusqlite::params![run, json!({"contextId":"fixture-context"}).to_string()],
        )
        .unwrap();
    }
    let request = ReconcileOperation {
        delivery_id: old.delivery_id.clone(),
        operation: send.clone(),
    };
    let counts = || -> (i64, i64) {
        (
            sql.query_row("SELECT count(*) FROM member_requests", [], |r| r.get(0))
                .unwrap(),
            sql.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                .unwrap(),
        )
    };
    let before = counts();
    assert_eq!(f.store.member_reconcile(&current, &request).unwrap(), saved);
    assert_eq!(f.store.member_reconcile(&current, &request).unwrap(), saved);
    let missing = ReconcileOperation {
        delivery_id: old.delivery_id.clone(),
        operation: member_operation(
            &old,
            "never-submitted",
            "message_send",
            json!({"recipient":f.human,"kind":"work.note","body":"不得补执行"}),
        ),
    };
    assert_eq!(
        f.store.member_reconcile(&current, &missing).unwrap()["error"]["code"],
        "not_executed"
    );
    assert_eq!(counts(), before);
    let mut bad = request.clone();
    bad.operation.input["body"] = json!("不同内容");
    assert!(f.store.member_reconcile(&current, &bad).is_err());
    bad = request.clone();
    bad.delivery_id = next.delivery_id.clone();
    assert!(f.store.member_reconcile(&current, &bad).is_err());
    bad = request.clone();
    bad.operation.operation_id = "invented-operation".into();
    assert!(f.store.member_reconcile(&current, &bad).is_err());
    sql.execute("UPDATE api_launches SET data=json_set(data,'$.contextId','foreign-context') WHERE run_id=?1",[&old.id]).unwrap();
    assert!(f.store.member_reconcile(&current, &request).is_err());
    sql.execute("UPDATE api_launches SET data=json_set(data,'$.contextId','fixture-context') WHERE run_id=?1",[&old.id]).unwrap();
    f.store
        .execute(
            "revoke-reconciliation",
            &Command::PermissionsUpdate {
                decision_id: None,
                team_id: f.team.id.clone(),
                revision: 2,
                grant: BTreeMap::new(),
                revoke: BTreeMap::from([(old.worker_id.clone(), vec![Permission::Communicate])]),
            },
        )
        .unwrap();
    assert!(f.store.member_reconcile(&current, &request).is_err());
    assert_eq!(counts(), before);
}

#[test]
fn cli_network_proxy_is_explicit_validated_and_frozen_with_the_run() {
    use atelier::connection::{ConnectionSpec, EgressProxy};
    let base = json!({"transport":"agent-cli","runtime":"pi","model":null,"image":null,"egress_hosts":["example.com"]});
    for proxy in [
        json!({"address":"localhost","port":80}),
        json!({"address":"127.0.0.1","port":80}),
        json!({"address":"169.254.169.254","port":80}),
        json!({"address":"192.168.5.2","port":0}),
        json!({"address":"192.168.5.2","port":65536}),
        json!({"address":"192.168.5.2","port":80,"password":"secret"}),
        json!("http://user:secret@192.168.5.2:80"),
    ] {
        let mut value = base.clone();
        value["egress_proxy"] = proxy;
        assert!(
            serde_json::from_value::<ConnectionSpec>(value)
                .map(|s| s.validate().is_err())
                .unwrap_or(true)
        );
    }
    let route = EgressProxy {
        address: "192.168.5.2".into(),
        port: 9567,
    };
    let (mut f, run) =
        prepared_cli_fixture_with_route(Some(vec!["example.com".into()]), Some(route.clone()));
    let config = f
        .store
        .execution_configuration(&run.configuration_id)
        .unwrap();
    let version = f
        .store
        .connection_version(&config.connection_version)
        .unwrap();
    let mut changed = version.specification.clone();
    let ConnectionSpec::AgentCli { egress_proxy, .. } = &mut changed else {
        unreachable!()
    };
    *egress_proxy = Some(EgressProxy {
        address: "10.0.0.1".into(),
        port: 8080,
    });
    let updated = f
        .store
        .execute(
            "new-proxy",
            &Command::ConnectionUpdate {
                id: version.connection_id,
                revision: 1,
                name: None,
                specification: Some(changed),
            },
        )
        .unwrap();
    assert_ne!(updated["version"]["id"], version.id);
    let context = f
        .store
        .runtime_context(
            "service",
            &run.task_id,
            &run.worker_id,
            &run.configuration_id,
            &run.purpose,
        )
        .unwrap();
    let resources = f
        .store
        .runtime_begin_cli_launch("service", &run.id, "fixture-engine", &context)
        .unwrap();
    assert_eq!(resources.egress_proxy, Some(route.clone()));
    assert_eq!(
        Store::open(f.dir.path())
            .unwrap()
            .cli_resources(&run.id)
            .unwrap()
            .unwrap()
            .egress_proxy,
        Some(route)
    );
}
