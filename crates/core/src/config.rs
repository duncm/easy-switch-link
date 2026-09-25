use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub follow: crate::follow::FollowConfig,
    pub version: u32,
    pub computers: BTreeMap<String, String>,
    pub groups: Vec<Group>,
    pub start_at_login: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 2,
            follow: Default::default(),
            computers: (1..=3)
                .map(|i| (format!("computer-{i}"), format!("Computer {i}")))
                .collect(),
            groups: Vec::new(),
            start_at_login: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub devices: Vec<Member>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub id: String,
    /// Logical computer ID -> one-based device channel.
    pub channels: BTreeMap<String, u8>,
    #[serde(default)]
    pub mode: SourceMode,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceMode {
    /// Prefer passive announcements/analytics; use temporary diversion as a fallback.
    #[default]
    Auto,
    /// Explicit fallback when the device exposes divertable Easy-Switch keys.
    Intercept,
    FollowOnly,
}

impl Config {
    pub fn migrate(mut self) -> Result<Self> {
        self.validate()?;
        if self.version == 1 {
            self.version = 2;
            self.follow = Default::default();
        }
        Ok(self)
    }
    /// IDs retain exact-match precedence; display names are case-insensitive.
    /// Resolve before planning so ambiguous names can never move any devices.
    pub fn resolve_computer(&self, selector: &str) -> Result<&str> {
        if let Some((id, _)) = self.computers.get_key_value(selector) {
            return Ok(id);
        }
        let name = selector.trim().to_lowercase();
        let matches: Vec<_> = self
            .computers
            .iter()
            .filter(|(_, label)| label.trim().to_lowercase() == name)
            .map(|(id, _)| id.as_str())
            .collect();
        match matches.as_slice() {
            [id] => Ok(id),
            [] => bail!(
                "Unknown computer {selector:?}. Available computers: {}",
                self.computers
                    .iter()
                    .map(|(id, label)| format!("{id} ({label})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => bail!(
                "Ambiguous computer name {selector:?}; use one of these IDs: {}",
                matches.join(", ")
            ),
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.version, 1 | 2),
            "Unsupported configuration version {}",
            self.version
        );
        ensure!(
            !self.computers.is_empty() && self.computers.len() <= 32,
            "Define between 1 and 32 computers"
        );
        ensure!(self.groups.len() <= 32, "Too many groups");
        self.follow.validate(self)?;
        let mut groups = BTreeSet::new();
        let mut devices = BTreeSet::new();
        for (id, label) in &self.computers {
            ensure!(
                !id.trim().is_empty()
                    && id.len() <= 128
                    && !label.trim().is_empty()
                    && label.len() <= 128,
                "Invalid computer name"
            );
        }
        for group in &self.groups {
            ensure!(
                !group.id.trim().is_empty() && group.id.len() <= 128 && groups.insert(&group.id),
                "Duplicate or empty group ID"
            );
            ensure!(group.devices.len() <= 32, "Too many devices in group");
            for member in &group.devices {
                ensure!(
                    !member.id.is_empty() && member.id.len() <= 256 && devices.insert(&member.id),
                    "A device must belong to only one group"
                );
                ensure!(
                    !member.channels.is_empty(),
                    "Device {} has no mappings",
                    member.id
                );
                let mut channels = BTreeSet::new();
                for (computer, channel) in &member.channels {
                    ensure!(
                        self.computers.contains_key(computer),
                        "Unknown computer {computer}"
                    );
                    ensure!(
                        (1..=3).contains(channel) && channels.insert(channel),
                        "Device {} has an invalid or ambiguous channel mapping",
                        member.id
                    );
                }
            }
        }
        Ok(())
    }

    pub fn member(&self, id: &str) -> Option<&Member> {
        self.groups
            .iter()
            .flat_map(|g| &g.devices)
            .find(|d| d.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn computer_names_resolve_without_ambiguous_or_partial_switches() {
        let mut config = Config {
            computers: [
                ("computer-1".into(), "test-alpha".into()),
                ("computer-2".into(), "test-bravo".into()),
                ("computer-3".into(), "Office PC".into()),
            ]
            .into(),
            ..Config::default()
        };
        assert_eq!(config.resolve_computer("computer-2").unwrap(), "computer-2");
        assert_eq!(config.resolve_computer("test-bravo").unwrap(), "computer-2");
        assert_eq!(config.resolve_computer("TEST-ALPHA").unwrap(), "computer-1");
        assert_eq!(
            config.resolve_computer(" office pc ").unwrap(),
            "computer-3"
        );
        assert!(config.resolve_computer("test-b").is_err());
        assert!(config.resolve_computer("").is_err());
        config
            .computers
            .insert("computer-3".into(), "TEST-BRAVO".into());
        let error = config
            .resolve_computer("test-bravo")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Ambiguous")
                && error.contains("computer-2")
                && error.contains("computer-3")
        );
        assert_eq!(config.resolve_computer("computer-2").unwrap(), "computer-2");
        config
            .computers
            .insert("computer-3".into(), "computer-1".into());
        assert_eq!(config.resolve_computer("computer-1").unwrap(), "computer-1");
    }
    #[test]
    fn reject_ambiguous_mapping_and_membership() {
        let member = Member {
            id: "unit:a".into(),
            channels: [("computer-1".into(), 1), ("computer-2".into(), 1)].into(),
            mode: SourceMode::Auto,
        };
        let mut config = Config {
            groups: vec![Group {
                id: "desk".into(),
                devices: vec![member],
            }],
            ..Config::default()
        };
        assert!(config.validate().is_err());
        config.groups[0].devices[0]
            .channels
            .insert("computer-2".into(), 2);
        assert!(config.validate().is_ok());
        let mut duplicate = config.groups[0].clone();
        duplicate.id = "other".into();
        config.groups.push(duplicate);
        assert!(config.validate().is_err());
    }
    #[test]
    fn roundtrip_and_reject_unknown_fields() {
        let config = Config::default();
        assert_eq!(
            toml::from_str::<Config>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );
        assert!(toml::from_str::<Config>("verison = 1").is_err());
    }
}
