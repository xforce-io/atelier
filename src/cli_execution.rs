//! CLI-specific preparation for the shared member execution driver.
use crate::{
    Error, Result, cli_environment::CliEnvironment, database::DatabaseClient,
    execution_context::ExecutionContext,
};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub(crate) struct Prepared {
    pub environment: CliEnvironment,
    pub engine: String,
    // Serializes native credential refresh with explicit login/preparation.
    // Session volumes remain per-context; no host credentials are copied.
    _lock: File,
}

pub(crate) async fn prepare(
    client: &DatabaseClient,
    configuration: &str,
    context: &ExecutionContext,
    workspace: &Path,
) -> Result<Prepared> {
    let path = workspace.to_path_buf();
    let lock = tokio::task::spawn_blocking(move || crate::cli_environment::lock(&path))
        .await
        .map_err(|_| Error::Unavailable("CLI 环境锁线程退出".into()))??;
    let id = configuration.to_string();
    let environment = client
        .call(move |store| {
            let environment = store.cli_environment(&id)?;
            if let Some(environment) = &environment {
                crate::connection_probe::ensure_idle(&store.connection, &environment.worker_id)?;
            }
            Ok(environment)
        })
        .await?
        .ok_or_else(|| Error::Unavailable("先准备该 Worker 的专用 CLI 环境".into()))?;
    if environment.state != "prepared" || !environment.login_material_ready {
        return Err(Error::Unavailable(
            "专用 CLI 登录尚未完成；查询 connection show".into(),
        ));
    }
    let engine = crate::cli_resources::engine_identity().await?;
    if environment.engine_id.as_ref() != Some(&engine) {
        return Err(Error::Unavailable("CLI 环境所属 Docker 引擎已变化".into()));
    }
    let image: Value = serde_json::from_slice(
        &crate::cli_resources::docker(&["image", "inspect", &environment.image]).await?,
    )?;
    if image.as_array().is_none_or(|images| images.len() != 1)
        || image[0]["Os"] != "linux"
        || image[0]["Config"]["Labels"]["atelier.milkie"] != crate::channel::MILKIE_COMMIT
        || image[0]["Config"]["Labels"]["atelier.adapter.protocol"] != "2"
    {
        return Err(Error::Unavailable(
            "CLI 固定镜像不可用或与当前接入不兼容".into(),
        ));
    }
    let saved = environment.clone();
    let native = context.clone();
    let path = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        saved.validate_storage(&path)?;
        if !crate::cli_login::login_material(&saved, &path) {
            return Err(Error::Unavailable("Worker 专用登录材料缺失或不安全".into()));
        }
        native.prepare_cli_directories(&path)
    })
    .await
    .map_err(|_| Error::Unavailable("CLI 存储准备线程退出".into()))??;
    Ok(Prepared {
        environment,
        engine,
        _lock: lock,
    })
}

pub(crate) fn bootstrap(
    prepared: &Prepared,
    resources: &crate::cli_resources::CliResources,
    context: &ExecutionContext,
    workspace: &Path,
    skill: &Path,
) -> Value {
    let (native, ledger) = context.directories(workspace);
    json!({"runId":resources.run_id,"workspaceId":resources.workspace_id,"ownershipToken":resources.ownership_token,
        "engineId":resources.engine_id,"image":resources.image,"hosts":resources.egress_hosts,
        "nativeDirectory":native,"ledgerDirectory":ledger,"configDirectory":prepared.environment.directory(workspace).join("login"),"skillDirectory":skill})
}

/// Every mounted file is generated from the installed core tool catalogue.
/// Refuse an existing Run directory rather than adopting foreign content.
pub(crate) fn write_skill(workspace: &Path, run: &str, description: &Value) -> Result<PathBuf> {
    let parent = workspace.join("run-skills");
    crate::execution_context::private_directory(&parent, true)?;
    let root = parent.join(run);
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root)?;
    let files = description["files"]
        .as_object()
        .ok_or_else(|| Error::Invalid("成员 Skill 文件缺失".into()))?;
    for (name, body) in files {
        if Path::new(name)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(Error::Invalid("成员 Skill 文件路径无效".into()));
        }
        let destination = root.join(name);
        if let Some(parent) = destination.parent() {
            builder.recursive(true).create(parent)?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(destination)?;
        file.write_all(
            body.as_str()
                .ok_or_else(|| Error::Invalid("成员 Skill 文件无效".into()))?
                .as_bytes(),
        )?;
        file.sync_all()?;
    }
    File::open(&root)?.sync_all()?;
    File::open(&parent)?.sync_all()?;
    Ok(root)
}
