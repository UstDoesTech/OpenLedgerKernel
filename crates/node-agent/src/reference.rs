//! The reference to the secret, in the form each cloud uses. They are checked when the node
//! starts, so a typo stops it before it makes a database it cannot store the password for.

fn name_chars(s: &str, extra: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// `arn:aws:secretsmanager:<region>:<account>:secret:<name>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsRef {
    pub arn: String,
    pub region: String,
}

pub fn parse_aws(id: &str) -> Result<AwsRef, String> {
    let bad = || {
        format!(
            "{id:?} is not a Secrets Manager ARN like arn:aws:secretsmanager:eu-west-2:111111111111:secret:name-AbCdEf"
        )
    };
    let parts: Vec<&str> = id.splitn(7, ':').collect();
    if parts.len() != 7
        || parts[0] != "arn"
        || parts[1] != "aws"
        || parts[2] != "secretsmanager"
        || parts[5] != "secret"
    {
        return Err(bad());
    }
    let region = parts[3];
    let region_ok = {
        let bits: Vec<&str> = region.split('-').collect();
        bits.len() == 3
            && bits[0].len() == 2
            && bits[0].chars().all(|c| c.is_ascii_lowercase())
            && !bits[1].is_empty()
            && bits[1].chars().all(|c| c.is_ascii_lowercase())
            && bits[2].chars().all(|c| c.is_ascii_digit())
            && !bits[2].is_empty()
    };
    let account_ok = parts[4].len() == 12 && parts[4].chars().all(|c| c.is_ascii_digit());
    if !region_ok || !account_ok || !name_chars(parts[6], "/_+=.@-") {
        return Err(bad());
    }
    Ok(AwsRef {
        arn: id.to_string(),
        region: region.to_string(),
    })
}

/// `https://<vault>.vault.azure.net/secrets/<name>`, with no version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureRef {
    pub url: String,
}

pub fn parse_azure(id: &str) -> Result<AzureRef, String> {
    let bad = || {
        format!(
            "{id:?} is not a Key Vault secret address like https://vault-name.vault.azure.net/secrets/name"
        )
    };
    let rest = id.strip_prefix("https://").ok_or_else(bad)?;
    let (host, path) = rest.split_once('/').ok_or_else(bad)?;
    let vault = host.strip_suffix(".vault.azure.net").ok_or_else(bad)?;
    let name = path.strip_prefix("secrets/").ok_or_else(bad)?;
    let vault_ok =
        (3..=24).contains(&vault.len()) && name_chars(vault, "-") && !vault.starts_with('-');
    if !vault_ok || name.len() > 127 || !name_chars(name, "-") {
        return Err(bad());
    }
    Ok(AzureRef {
        url: id.to_string(),
    })
}

/// `projects/<project>/secrets/<name>`, with no version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcpRef {
    pub resource: String,
}

pub fn parse_gcp(id: &str) -> Result<GcpRef, String> {
    let bad =
        || format!("{id:?} is not a Secret Manager name like projects/project-id/secrets/name");
    let parts: Vec<&str> = id.split('/').collect();
    if parts.len() != 4 || parts[0] != "projects" || parts[2] != "secrets" {
        return Err(bad());
    }
    let project = parts[1];
    let project_ok = (6..=30).contains(&project.len())
        && project
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && project.starts_with(|c: char| c.is_ascii_lowercase());
    if !project_ok || parts[3].len() > 255 || !name_chars(parts[3], "_-") {
        return Err(bad());
    }
    Ok(GcpRef {
        resource: id.to_string(),
    })
}
