// Codeplug parsing: the MS ships its whole configuration as a single TOML
// document (config_version "0.7") via GetConfig. We only read the "radio
// programming" we need for the home cycler: folders and talkgroups. Every other
// section of the document is ignored (serde drops unknown keys).
//
// Semantics mirror the browser tetra-tn-web-ui codeplug tree: talkgroups are
// grouped by their folder id, folders are ordered by (order, name), talkgroups
// within a folder by (order, name), and talkgroups with no/blank folder fall
// into a synthetic "Other" folder.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Talkgroup {
    pub gssi: u32,
    pub name: String,
    /// On-air class of usage (0..7); maps to TNMM ClassOfUsage(N+1) when attaching.
    pub class_of_usage: u8,
}

#[derive(Debug, Clone)]
pub struct Folder {
    #[allow(dead_code)]
    pub id: String,
    pub name: String,
    pub talkgroups: Vec<Talkgroup>,
}

#[derive(Debug, Clone, Default)]
pub struct Codeplug {
    pub folders: Vec<Folder>,
    #[allow(dead_code)] // used by name_of, handy for later screens/logs
    names: HashMap<u32, String>,
}

#[derive(Deserialize)]
struct Doc {
    #[serde(default)]
    folder: Vec<FolderDef>,
    #[serde(default)]
    talkgroup: Vec<TalkgroupDef>,
}

#[derive(Deserialize, Clone)]
struct FolderDef {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    order: i64,
}

#[derive(Deserialize)]
struct TalkgroupDef {
    gssi: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    class_of_usage: Option<u8>,
    #[serde(default)]
    order: i64,
}

impl Codeplug {
    /// Parse a codeplug from the full MS config TOML. Returns None if the TOML is
    /// unparseable or carries no talkgroups.
    pub fn parse(toml_str: &str) -> Option<Codeplug> {
        let doc: Doc = toml::from_str(toml_str).ok()?;
        if doc.talkgroup.is_empty() {
            return None;
        }

        let mut names = HashMap::new();
        for t in &doc.talkgroup {
            let name = if t.name.is_empty() {
                format!("TG {}", t.gssi)
            } else {
                t.name.clone()
            };
            names.insert(t.gssi, name);
        }

        let mut folder_defs = doc.folder.clone();
        folder_defs.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));

        let mut by_folder: HashMap<String, (i64, Vec<(i64, Talkgroup)>)> = HashMap::new();
        for t in &doc.talkgroup {
            let fid = t
                .folder
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "__none".to_string());
            let tg = Talkgroup {
                gssi: t.gssi,
                name: names[&t.gssi].clone(),
                class_of_usage: t.class_of_usage.unwrap_or(0),
            };
            by_folder.entry(fid).or_default().1.push((t.order, tg));
        }

        // Folder display order: defined folders (that have talkgroups) first, then
        // any remaining folder ids sorted, then "Other" last.
        let mut order: Vec<String> = folder_defs
            .iter()
            .map(|f| f.id.clone())
            .filter(|id| by_folder.contains_key(id))
            .collect();
        let mut rest: Vec<String> = by_folder
            .keys()
            .filter(|id| !order.contains(id) && id.as_str() != "__none")
            .cloned()
            .collect();
        rest.sort();
        order.append(&mut rest);
        if by_folder.contains_key("__none") {
            order.push("__none".to_string());
        }

        let name_of = |fid: &str| -> String {
            if fid == "__none" {
                "Other".to_string()
            } else {
                folder_defs
                    .iter()
                    .find(|f| f.id == fid)
                    .map(|f| {
                        if f.name.is_empty() {
                            fid.to_string()
                        } else {
                            f.name.clone()
                        }
                    })
                    .unwrap_or_else(|| fid.to_string())
            }
        };

        let folders = order
            .into_iter()
            .filter_map(|fid| {
                let (_, mut tgs) = by_folder.remove(&fid)?;
                tgs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
                Some(Folder {
                    name: name_of(&fid),
                    id: fid,
                    talkgroups: tgs.into_iter().map(|(_, t)| t).collect(),
                })
            })
            .collect();

        Some(Codeplug { folders, names })
    }

    #[allow(dead_code)] // used in tests and by later screens/logs
    pub fn name_of(&self, gssi: u32) -> String {
        self.names
            .get(&gssi)
            .cloned()
            .unwrap_or_else(|| format!("TG {gssi}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
config_version = "0.7"

[net_info]
mcc = 901
mnc = 9999

[[folder]]
id    = "work"
name  = "Work"
order = 0

[[folder]]
id    = "ops"
name  = "Operations"
order = 1

[[talkgroup]]
gssi = 102
name = "Field Ops"
folder = "work"
class_of_usage = 0
order = 1

[[talkgroup]]
gssi = 101
name = "Dispatch"
folder = "work"
class_of_usage = 0
order = 0

[[talkgroup]]
gssi = 300
name = "Emergency"
folder = "ops"
class_of_usage = 3
order = 1

[[talkgroup]]
gssi = 91
name = "Loose"
"#;

    #[test]
    fn parses_folders_and_talkgroups() {
        let cp = Codeplug::parse(SAMPLE).expect("codeplug parses");
        // Work, Operations, Other
        assert_eq!(cp.folders.len(), 3);
        assert_eq!(cp.folders[0].name, "Work");
        // Sorted within folder by order: Dispatch (0) before Field Ops (1).
        assert_eq!(cp.folders[0].talkgroups[0].name, "Dispatch");
        assert_eq!(cp.folders[0].talkgroups[1].name, "Field Ops");
        assert_eq!(cp.folders[1].name, "Operations");
        assert_eq!(cp.folders[2].name, "Other");
        assert_eq!(cp.folders[2].talkgroups[0].gssi, 91);
        assert_eq!(cp.name_of(300), "Emergency");
        assert_eq!(cp.name_of(999), "TG 999");
    }

    #[test]
    fn none_or_empty_codeplug() {
        assert!(Codeplug::parse("config_version = \"0.7\"").is_none());
        assert!(Codeplug::parse("not valid toml {{{").is_none());
    }
}
