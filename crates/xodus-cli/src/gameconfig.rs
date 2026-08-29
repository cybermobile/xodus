//! `MicrosoftGame.config` - the GDK package manifest shipped at the root of
//! every MSIXVC. Parsed leniently: every field is optional so config-version
//! differences never fail a launch, they just yield less identity data.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct MicrosoftGameConfig {
    pub identity: Option<Identity>,
    pub executable_list: Option<ExecutableList>,
    pub title_id: Option<String>,
    #[serde(rename = "MSAAppId")]
    pub msa_app_id: Option<String>,
    pub store_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Identity {
    #[serde(rename = "@Name")]
    pub name: Option<String>,
    #[serde(rename = "@Publisher")]
    pub publisher: Option<String>,
    #[serde(rename = "@Version")]
    pub version: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ExecutableList {
    #[serde(rename = "Executable", default)]
    pub executables: Vec<Executable>,
}

#[derive(Deserialize, Debug)]
pub struct Executable {
    #[serde(rename = "@Name")]
    pub name: Option<String>,
    #[serde(rename = "@TargetDeviceFamily")]
    pub target_device_family: Option<String>,
}

impl MicrosoftGameConfig {
    pub fn parse(xml: &str) -> Result<Self, quick_xml::DeError> {
        quick_xml::de::from_str(xml)
    }

    /// Declared executable names in declaration order, PC-targeted entries
    /// first - GDK launches the first (matching) declared executable.
    pub fn declared_executables(&self) -> Vec<&str> {
        let Some(list) = &self.executable_list else {
            return vec![];
        };
        let mut names: Vec<(&str, bool)> = list
            .executables
            .iter()
            .filter_map(|e| {
                let name = e.name.as_deref()?;
                let pc = e
                    .target_device_family
                    .as_deref()
                    .is_none_or(|f| f.eq_ignore_ascii_case("PC"));
                Some((name, pc))
            })
            .collect();
        names.sort_by_key(|(_, pc)| !pc);
        names.into_iter().map(|(name, _)| name).collect()
    }
}

/// Finds `MicrosoftGame.config` in the install root, tolerating the
/// case-insensitive names the NTFS extraction may have produced.
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.eq_ignore_ascii_case("MicrosoftGame.config"))
        {
            return Some(entry.path());
        }
    }
    None
}

pub fn load_from_dir(dir: &Path) -> Option<MicrosoftGameConfig> {
    let path = find_config_file(dir)?;
    let xml = match std::fs::read_to_string(&path) {
        Ok(xml) => xml,
        Err(err) => {
            eprintln!("warning: cannot read `{}`: {}", path.display(), err);
            return None;
        }
    };
    match MicrosoftGameConfig::parse(&xml) {
        Ok(config) => Some(config),
        Err(err) => {
            eprintln!("warning: cannot parse `{}`: {}", path.display(), err);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MicrosoftGameConfig;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Game configVersion="1">
  <Identity Name="41336MicrosoftStudios.SampleGame" Publisher="CN=Microsoft Studios" Version="1.2.3.0" />
  <ExecutableList>
    <Executable Name="Launcher.exe" Id="Launcher" TargetDeviceFamily="Tool" />
    <Executable Name="SampleGame.exe" Id="Game" TargetDeviceFamily="PC" />
  </ExecutableList>
  <ShellVisuals DefaultDisplayName="Sample Game" />
  <StoreId>9NBLGGH4R315</StoreId>
  <TitleId>1F5D3B7A</TitleId>
  <MSAAppId>000000004C2159A3</MSAAppId>
</Game>"#;

    #[test]
    fn parses_identity_and_ids() {
        let config = MicrosoftGameConfig::parse(SAMPLE).unwrap();
        let identity = config.identity.as_ref().unwrap();
        assert_eq!(
            identity.name.as_deref(),
            Some("41336MicrosoftStudios.SampleGame")
        );
        assert_eq!(identity.version.as_deref(), Some("1.2.3.0"));
        assert_eq!(config.title_id.as_deref(), Some("1F5D3B7A"));
        assert_eq!(config.msa_app_id.as_deref(), Some("000000004C2159A3"));
        assert_eq!(config.store_id.as_deref(), Some("9NBLGGH4R315"));
    }

    #[test]
    fn declared_executables_prefer_pc_targets() {
        let config = MicrosoftGameConfig::parse(SAMPLE).unwrap();
        assert_eq!(
            config.declared_executables(),
            vec!["SampleGame.exe", "Launcher.exe"]
        );
    }

    #[test]
    fn unknown_fields_and_missing_sections_are_tolerated() {
        let config =
            MicrosoftGameConfig::parse(r#"<Game configVersion="0"><Whatever /></Game>"#).unwrap();
        assert!(config.identity.is_none());
        assert!(config.declared_executables().is_empty());
    }
}
