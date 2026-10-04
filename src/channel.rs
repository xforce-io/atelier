//! Private adapter pipe. The service supplies the scope and member binding;
//! received IDs are assertions to verify, never identities to impersonate.
use crate::{
    Error, Result,
    database::DatabaseClient,
    member::{MemberBinding, ToolOperation},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::watch,
};

pub const MILKIE_COMMIT: &str = "da7767790bcb38e30aa49910ad05fba3670689ca";
const MAX_FRAME: usize = 2 * 1024 * 1024;
const MAX_FRAMES: u64 = 1024;
const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scope {
    pub task_id: String,
    pub run_id: String,
    pub delivery_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Frame {
    protocol_version: u8,
    request_id: String,
    task_id: String,
    run_id: String,
    delivery_id: String,
    seq: u64,
    kind: String,
    payload: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    pub milkie_commit: String,
    pub transport: String,
    pub skill_digest: String,
    pub builtin_tools_disabled: bool,
    pub stable_tool_call_ids: bool,
    pub native_checkpoint: bool,
    pub private_tools: bool,
}

impl Capabilities {
    pub fn cli(skill: &str) -> Self {
        Self {
            transport: "agent-cli".into(),
            ..Self::api(skill)
        }
    }
    pub fn api(skill: &str) -> Self {
        Self {
            milkie_commit: MILKIE_COMMIT.into(),
            transport: "api".into(),
            skill_digest: format!("{:x}", Sha256::digest(skill.as_bytes())),
            builtin_tools_disabled: true,
            stable_tool_call_ids: true,
            native_checkpoint: true,
            private_tools: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Terminal {
    pub stop_reason: String,
    pub stop_code: Option<String>,
    pub native_stop_reason: String,
    pub recovered_operations: u32,
}
impl Terminal {
    fn validate(&self) -> Result<()> {
        if ![
            "completed",
            "cancelled",
            "deadline",
            "budget_exhausted",
            "failed",
        ]
        .contains(&self.stop_reason.as_str())
            || self.native_stop_reason.is_empty()
            || self.native_stop_reason.len() > 128
            || self.stop_code.as_ref().is_some_and(|s| s.len() > 128)
            // Recovery can span multiple earlier Runs. It is bounded by the
            // private-channel frame budget, not the new Run's 100 tool calls.
            || self.recovered_operations > MAX_FRAMES as u32
        {
            return Err(protocol_error());
        }
        Ok(())
    }
}

fn protocol_error() -> Error {
    Error::Invalid("成员通道协议无效；停止本次接入并保留待核对操作".into())
}

struct Reader<R> {
    stream: BufReader<R>,
    partial: Vec<u8>,
    hashes: BTreeMap<u64, [u8; 32]>,
    bytes: usize,
}
impl<R: AsyncRead + Unpin> Reader<R> {
    fn new(stream: R) -> Self {
        Self {
            stream: BufReader::new(stream),
            partial: Vec::new(),
            hashes: BTreeMap::new(),
            bytes: 0,
        }
    }
    // Partial bytes live in self so select! cancellation never loses a prefix.
    async fn receive(&mut self, scope: &Scope) -> Result<(Frame, bool)> {
        loop {
            let chunk = self.stream.fill_buf().await?;
            if chunk.is_empty() {
                return Err(Error::Unavailable(
                    "成员通道已关闭，终态或工具回复可能丢失".into(),
                ));
            }
            let end = chunk.iter().position(|b| *b == b'\n');
            let count = end.map_or(chunk.len(), |n| n + 1);
            if self.partial.len() + count > MAX_FRAME || self.bytes + count > MAX_BYTES {
                return Err(protocol_error());
            }
            self.partial.extend_from_slice(&chunk[..count]);
            self.bytes += count;
            self.stream.consume(count);
            if end.is_none() {
                continue;
            }
            let frame: Frame =
                serde_json::from_slice(&self.partial).map_err(|_| protocol_error())?;
            self.partial.clear();
            if frame.protocol_version != 1
                || frame.task_id != scope.task_id
                || frame.run_id != scope.run_id
                || frame.delivery_id != scope.delivery_id
                || frame.request_id.is_empty()
                || frame.request_id.len() > 256
                || frame.seq == 0
                || frame.seq > MAX_FRAMES
            {
                return Err(protocol_error());
            }
            let hash: [u8; 32] = Sha256::digest(serde_json::to_vec(&frame)?).into();
            if let Some(previous) = self.hashes.get(&frame.seq) {
                if *previous != hash {
                    return Err(protocol_error());
                }
                return Ok((frame, true));
            }
            if frame.seq != self.hashes.len() as u64 + 1 {
                return Err(protocol_error());
            }
            self.hashes.insert(frame.seq, hash);
            return Ok((frame, false));
        }
    }
}
struct Writer<W> {
    stream: W,
    scope: Scope,
    seq: u64,
    bytes: usize,
}
impl<W: AsyncWrite + Unpin> Writer<W> {
    async fn send(&mut self, request_id: &str, kind: &str, payload: Value) -> Result<()> {
        if self.seq >= MAX_FRAMES {
            return Err(protocol_error());
        }
        self.seq += 1;
        let frame = Frame {
            protocol_version: 1,
            request_id: request_id.into(),
            task_id: self.scope.task_id.clone(),
            run_id: self.scope.run_id.clone(),
            delivery_id: self.scope.delivery_id.clone(),
            seq: self.seq,
            kind: kind.into(),
            payload,
        };
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_FRAME || self.bytes + bytes.len() > MAX_BYTES {
            return Err(protocol_error());
        }
        self.bytes += bytes.len();
        tokio::time::timeout(Duration::from_secs(10), async {
            self.stream.write_all(&bytes).await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| Error::Unavailable("成员通道写入超时".into()))??;
        Ok(())
    }
}

/// Drives one already-owned adapter. It neither spawns a business stage nor
/// marks a delivery handled. Caller must inspect resources after any return.
pub async fn serve<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    input: R,
    output: W,
    database: DatabaseClient,
    binding: MemberBinding,
    configuration: ChannelConfiguration,
    mut stop: watch::Receiver<bool>,
) -> Result<Terminal> {
    let ChannelConfiguration {
        scope,
        capabilities,
        start,
    } = configuration;
    // Compare caller-supplied pipe scope to the authoritative bound Run before
    // disclosing start data, even though the service is the trusted caller.
    let bound = binding.clone();
    let (actual, cli) = database
        .call(move |store| {
            let scope = store.member_scope(&bound)?;
            let run = store.run(&scope.run_id)?;
            let configuration = store.execution_configuration(&run.configuration_id)?;
            let version = store.connection_version(&configuration.connection_version)?;
            Ok((
                scope,
                matches!(
                    version.specification,
                    crate::connection::ConnectionSpec::AgentCli { .. }
                ),
            ))
        })
        .await?;
    let skill = start
        .get("skill")
        .and_then(Value::as_str)
        .ok_or_else(protocol_error)?;
    if actual != scope
        || capabilities
            != if cli {
                Capabilities::cli(skill)
            } else {
                Capabilities::api(skill)
            }
    {
        return Err(protocol_error());
    }
    let mut reader = Reader::new(input);
    let mut writer = Writer {
        stream: output,
        scope: scope.clone(),
        seq: 0,
        bytes: 0,
    };
    writer
        .send("hello", "hello", serde_json::to_value(&capabilities)?)
        .await?;
    let (ready, _) = tokio::time::timeout(Duration::from_secs(10), reader.receive(&scope))
        .await
        .map_err(|_| Error::Unavailable("成员接入就绪超时".into()))??;
    if ready.kind != "ready"
        || ready.request_id != "hello"
        || serde_json::from_value::<Capabilities>(ready.payload).map_err(|_| protocol_error())?
            != capabilities
    {
        return Err(protocol_error());
    }
    let mut stopping = *stop.borrow();
    let run_deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    let mut deadline = if stopping {
        tokio::time::Instant::now() + Duration::from_secs(10)
    } else {
        run_deadline
    };
    if stopping {
        writer
            .send("stop", "stop", json!({"reason":"runtime_stop"}))
            .await?;
    } else {
        writer.send("start", "start", start).await?;
    }
    loop {
        let next = tokio::select! {
            _=tokio::time::sleep_until(deadline) => {
                if stopping { return Err(Error::Unavailable("成员未在停止期限内报告终态；需要资源核对".into())); }
                stopping=true;
                deadline=tokio::time::Instant::now()+Duration::from_secs(10);
                writer.send("stop","stop",json!({"reason":"deadline"})).await?;
                continue;
            }
            changed=stop.changed(), if !stopping => {
                if changed.is_err() || *stop.borrow() {
                    stopping=true;
                    deadline=tokio::time::Instant::now()+Duration::from_secs(10);
                    writer.send("stop","stop",json!({"reason":"runtime_stop"})).await?;
                }
                continue;
            }
            frame=reader.receive(&scope)=>frame?,
        };
        let (frame, duplicate) = next;
        match frame.kind.as_str() {
            "ready" if duplicate => {}
            "tool.request" => {
                if stopping {
                    return Err(Error::Conflict("停止中的成员接入不能提交新工具请求".into()));
                }
                let operation: ToolOperation =
                    serde_json::from_value(frame.payload).map_err(|_| protocol_error())?;
                if operation.operation_id != frame.request_id {
                    return Err(protocol_error());
                }
                let bound = binding.clone();
                // A lost pipe never cancels a database job already accepted.
                // Repeated frames still recheck current authorization in core.
                let result = database.member_call(bound, operation).await?;
                writer
                    .send(&frame.request_id, "tool.result", result)
                    .await?;
            }
            "tool.reconcile" => {
                if stopping {
                    return Err(Error::Conflict("停止中的接入不能核对旧调用".into()));
                }
                let request: crate::member::ReconcileOperation =
                    serde_json::from_value(frame.payload).map_err(|_| protocol_error())?;
                if request.operation.operation_id != frame.request_id {
                    return Err(protocol_error());
                }
                let bound = binding.clone();
                let result = database
                    .call(move |store| store.member_reconcile(&bound, &request))
                    .await?;
                writer
                    .send(&frame.request_id, "tool.result", result)
                    .await?;
            }
            "progress" | "report" => {
                if serde_json::to_vec(&frame.payload)?.len() > 16 * 1024 {
                    return Err(protocol_error());
                }
                // Progress is advisory data, not a business transition.
            }
            "terminal" => {
                if frame.request_id != "terminal" {
                    return Err(protocol_error());
                }
                let terminal: Terminal =
                    serde_json::from_value(frame.payload).map_err(|_| protocol_error())?;
                terminal.validate()?;
                return Ok(terminal);
            }
            _ => return Err(protocol_error()),
        }
    }
}

pub struct ChannelConfiguration {
    pub scope: Scope,
    pub capabilities: Capabilities,
    pub start: Value,
}
