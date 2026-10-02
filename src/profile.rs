use crate::{Error, Result, content::digest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationProfile {
    pub name: String,
    pub check_id: String,
    pub image: String,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileRecord {
    pub id: String,
    pub digest: String,
    pub specification: VerificationProfile,
}

impl VerificationProfile {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.check_id.is_empty()
            || self.check_id.len() > 128
            || !self
                .check_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            return Err(Error::Invalid("检验配置名称或 checkId 无效".into()));
        }
        validate_image(&self.image)?;
        if self.argv.is_empty()
            || self.argv.len() > 32
            || self
                .argv
                .iter()
                .any(|arg| arg.is_empty() || arg.len() > 4096 || arg.contains('\0'))
        {
            return Err(Error::Invalid("检查命令须为有界的明确 argv".into()));
        }
        Ok(())
    }
    pub fn record(&self) -> Result<ProfileRecord> {
        self.validate()?;
        let digest = digest(&serde_json::to_vec(self)?);
        Ok(ProfileRecord {
            id: digest.clone(),
            digest,
            specification: self.clone(),
        })
    }
}

pub fn validate_image(image: &str) -> Result<()> {
    let digest = if let Some((repository, hash)) = image.split_once("@sha256:") {
        if repository.is_empty()
            || repository.starts_with('-')
            || !repository
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._:/-".contains(&c))
        {
            return Err(Error::Invalid("检查镜像名称无效".into()));
        }
        hash
    } else {
        image
            .strip_prefix("sha256:")
            .ok_or_else(|| Error::Invalid("检查镜像须固定完整 sha256，不能使用可变 tag".into()))?
    };
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Invalid("检查镜像摘要无效".into()));
    }
    Ok(())
}
