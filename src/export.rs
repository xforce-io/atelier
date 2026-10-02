//! Export only immutable artifact files; never include workspace state or credentials.
use crate::{Error, Result, content, store::Store};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path};

fn empty_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_dir()
                || meta.file_type().is_symlink()
                || fs::read_dir(path)?.next().is_some()
            {
                return Err(Error::Conflict(
                    "导出目标须为新目录或空目录，不覆盖文件、链接或已有内容".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
impl Store {
    /// A local filesystem copy, not a task mutation or a member tool.
    pub fn export_artifact(&self, id: &str, destination: &Path) -> Result<Value> {
        let artifact = self.artifact(id)?;
        if crate::candidate::manifest(&artifact.files)? != artifact.total_bytes
            || content::digest(&serde_json::to_vec(&artifact.files)?) != artifact.content_digest
        {
            return Err(Error::Conflict("产出清单损坏，不能导出".into()));
        }
        let name = destination
            .file_name()
            .ok_or_else(|| Error::Invalid("导出目标必须明确指定目录名".into()))?;
        // Require an existing parent; canonicalize before checking workspace boundaries.
        let parent = fs::canonicalize(
            destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        let target = parent.join(name);
        if target.starts_with(fs::canonicalize(&self.workspace_path)?) {
            return Err(Error::Conflict("不能将产出导入工作区内部".into()));
        }
        empty_destination(&target)?;
        // Validate every blob before preparing a copy. The manifest is bounded to 50 MiB.
        let bytes = artifact
            .files
            .iter()
            .map(|file| crate::candidate::read_blob(&self.workspace_path, file))
            .collect::<Result<Vec<_>>>()?;
        let stage = parent.join(format!(".atelier-export-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&stage)?;
        let result = (|| -> Result<Value> {
            for (entry, bytes) in artifact.files.iter().zip(&bytes) {
                let path = stage.join(&entry.path);
                fs::create_dir_all(path.parent().unwrap())?;
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(if entry.executable { 0o700 } else { 0o600 });
                }
                let mut file = options.open(&path)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            let mut directories = vec![stage.clone()];
            let mut i = 0;
            while i < directories.len() {
                for entry in fs::read_dir(&directories[i])? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        directories.push(entry.path());
                    }
                }
                i += 1;
            }
            for directory in directories.iter().rev() {
                fs::File::open(directory)?.sync_all()?;
            }
            empty_destination(&target)?;
            // On the supported macOS platform a directory rename cannot replace a
            // nonempty directory or a file, including one created after the check.
            fs::rename(&stage, &target)?;
            fs::File::open(&parent)?.sync_all()?;
            Ok(json!({"artifactId":artifact.id,"taskId":artifact.task_id,
                "taskRevision":artifact.task_revision,"contentDigest":artifact.content_digest,
                "partial":artifact.partial,"summary":artifact.summary,"files":artifact.files,
                "totalBytes":artifact.total_bytes,"destination":target,"status":"exported",
                "acceptanceChanged":false}))
        })();
        if stage.exists() {
            let _ = fs::remove_dir_all(&stage);
        }
        result
    }
}
