use crate::{
    Error, Result,
    content::digest,
    model::*,
    store::{immutable, load, new_id, revision, save, text},
};
use rusqlite::Connection as Database;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApiProtocol {
    AnthropicMessages,
    OpenaiChatCompletions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ConnectionSpec {
    Api {
        protocol: ApiProtocol,
        model: String,
        base_url: Option<String>,
    },
    AgentCli {
        runtime: String,
        model: Option<String>,
        image: Option<String>,
        // Preserve the serialized identity of existing unprepared versions.
        // Adding/changing a policy creates a new immutable connection version.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        egress_hosts: Option<Vec<String>>,
    },
}
impl ConnectionSpec {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Api {
                model, base_url, ..
            } => {
                text(model, "API 模型", 256)?;
                if let Some(base_url) = base_url {
                    let url = url::Url::parse(base_url)
                        .map_err(|_| Error::Invalid("API 地址无效".into()))?;
                    if base_url.len() > 2048
                        || url.scheme() != "https"
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.query().is_some()
                        || url.fragment().is_some()
                    {
                        return Err(Error::Invalid(
                            "API 地址须为 HTTPS，且不能含用户凭据、查询参数或片段".into(),
                        ));
                    }
                }
            }
            Self::AgentCli {
                runtime,
                model,
                image,
                egress_hosts,
            } => {
                text(runtime, "CLI runtime", 64)?;
                if !runtime
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                {
                    return Err(Error::Invalid("CLI runtime 格式无效".into()));
                }
                if let Some(model) = model {
                    text(model, "CLI 模型", 256)?;
                }
                if let Some(image) = image {
                    crate::profile::validate_image(image)?;
                }
                if let Some(hosts) = egress_hosts {
                    validate_egress_hosts(hosts)?;
                }
            }
        }
        Ok(())
    }
}

fn validate_egress_hosts(hosts: &[String]) -> Result<()> {
    let valid = !hosts.is_empty()
        && hosts.len() <= 64
        && hosts
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == hosts.len()
        && hosts.iter().all(|host| {
            host.len() <= 253
                && host.contains('.')
                && host.parse::<std::net::IpAddr>().is_err()
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                        && label.as_bytes()[0].is_ascii_alphanumeric()
                        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                })
        });
    if !valid {
        return Err(Error::Invalid(
            "CLI 出站策略须为 1–64 个不重复的小写精确域名，不接受 IP、通配符或 URL".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConnection {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub current_version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionVersion {
    pub id: String,
    pub connection_id: String,
    pub specification: ConnectionSpec,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfiguration {
    pub id: String,
    pub worker_id: String,
    pub connection_version: String,
}

fn version(connection_id: &str, specification: &ConnectionSpec) -> Result<ConnectionVersion> {
    specification.validate()?;
    let id = digest(&serde_json::to_vec(&(connection_id, specification))?);
    Ok(ConnectionVersion {
        id,
        connection_id: connection_id.into(),
        specification: specification.clone(),
    })
}

pub(crate) fn bind_worker(db: &Database, worker: &mut Worker, connection_id: &str) -> Result<()> {
    if worker.kind != WorkerKind::Agent {
        return Err(Error::Forbidden("人类成员不配置执行器".into()));
    }
    let connection: ModelConnection = load(db, "connections", connection_id)?;
    let version: ConnectionVersion = load(db, "connection_versions", &connection.current_version)?;
    version.specification.validate()?;
    let configuration = ExecutionConfiguration {
        id: digest(&serde_json::to_vec(&(&worker.id, &version.id))?),
        worker_id: worker.id.clone(),
        connection_version: version.id,
    };
    immutable(db, "execution_configs", &configuration.id, &configuration)?;
    worker.execution_config = Some(configuration.id);
    Ok(())
}

pub(crate) fn apply(db: &Database, command: &Command) -> Result<Value> {
    match command {
        Command::ConnectionCreate {
            name,
            specification,
        } => {
            text(name, "连接名称", 256)?;
            let id = new_id();
            let version = version(&id, specification)?;
            let connection = ModelConnection {
                id,
                name: name.clone(),
                revision: 1,
                current_version: version.id.clone(),
            };
            save(db, "connections", &connection.id, &connection)?;
            immutable(db, "connection_versions", &version.id, &version)?;
            Ok(json!({"connection":connection,"version":version,"readiness":"unchecked"}))
        }
        Command::ConnectionUpdate {
            id,
            revision: expected,
            name,
            specification,
        } => {
            let mut connection: ModelConnection = load(db, "connections", id)?;
            revision(connection.revision, *expected)?;
            if let Some(name) = name {
                text(name, "连接名称", 256)?;
                connection.name = name.clone();
            }
            if let Some(specification) = specification {
                let version = version(id, specification)?;
                immutable(db, "connection_versions", &version.id, &version)?;
                connection.current_version = version.id;
            }
            connection.revision += 1;
            save(db, "connections", id, &connection)?;
            let version: ConnectionVersion =
                load(db, "connection_versions", &connection.current_version)?;
            let diagnostic = crate::connection_probe::readiness(db, &connection)?;
            Ok(
                json!({"connection":connection,"version":version,"readiness":diagnostic["readiness"],"lastTest":diagnostic["lastTest"]}),
            )
        }
        _ => Err(Error::Invalid("不是连接配置操作".into())),
    }
}
