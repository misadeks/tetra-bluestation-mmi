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

#[derive(Debug, Clone)]
pub struct Scanlist {
    pub name: String,
    pub talkgroups: Vec<u32>,
}

/// External-network access point (PABX/PSTN gateway). `kind` is a UI label only
/// and has no on-air effect.
#[derive(Debug, Clone)]
pub struct Gateway {
    pub id: String,
    pub name: String,
    /// "pstn" | "pabx" (display label only).
    pub kind: String,
    pub gateway_issi: u32,
    /// Optional access-code digits prepended to a contact's number.
    pub prefix: String,
}

/// Phone-book entry. Exactly one target form: an on-network `issi`, or a
/// `number` dialled through a `gateway`.
#[derive(Debug, Clone)]
pub struct Contact {
    pub name: String,
    pub callsign: Option<String>,
    pub issi: Option<u32>,
    pub number: Option<String>,
    pub gateway: Option<String>,
    #[allow(dead_code)] // sort key; retained for parity with the codeplug
    pub order: i64,
}

/// The resolved on-air target of a contact.
#[derive(Debug, Clone, PartialEq)]
pub enum CallTarget {
    /// On-network individual call to this ISSI.
    Individual(u32),
    /// External (PABX/PSTN) call: dial the gateway ISSI, enclose the digits.
    External { gateway_ssi: u32, digits: String },
}

#[derive(Debug, Clone, Default)]
pub struct Codeplug {
    pub folders: Vec<Folder>,
    pub scanlists: Vec<Scanlist>,
    pub gateways: Vec<Gateway>,
    pub contacts: Vec<Contact>,
    #[allow(dead_code)] // used by name_of, handy for later screens/logs
    names: HashMap<u32, String>,
}

#[derive(Deserialize)]
struct Doc {
    #[serde(default)]
    folder: Vec<FolderDef>,
    #[serde(default)]
    talkgroup: Vec<TalkgroupDef>,
    #[serde(default)]
    scanlist: Vec<ScanlistDef>,
    #[serde(default)]
    gateway: Vec<GatewayDef>,
    #[serde(default)]
    contact: Vec<ContactDef>,
}

#[derive(Deserialize)]
struct GatewayDef {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    gateway_issi: u32,
    #[serde(default)]
    prefix: Option<String>,
}

#[derive(Deserialize)]
struct ContactDef {
    #[serde(default)]
    name: String,
    #[serde(default)]
    callsign: Option<String>,
    #[serde(default)]
    issi: Option<u32>,
    #[serde(default)]
    number: Option<String>,
    #[serde(default)]
    gateway: Option<String>,
    #[serde(default)]
    order: i64,
}

#[derive(Deserialize)]
struct ScanlistDef {
    #[serde(default)]
    name: String,
    #[serde(default)]
    talkgroups: Vec<u32>,
    #[serde(default)]
    order: i64,
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
        if doc.talkgroup.is_empty() && doc.contact.is_empty() && doc.gateway.is_empty() {
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

        let mut scan_defs = doc.scanlist;
        scan_defs.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        let scanlists = scan_defs
            .into_iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| Scanlist {
                name: s.name,
                talkgroups: s.talkgroups,
            })
            .collect();

        let gateways = doc
            .gateway
            .into_iter()
            .filter(|g| !g.id.is_empty())
            .map(|g| Gateway {
                name: if g.name.is_empty() { g.id.clone() } else { g.name },
                id: g.id,
                kind: g.kind,
                gateway_issi: g.gateway_issi,
                prefix: g.prefix.unwrap_or_default(),
            })
            .collect();

        let mut contact_defs = doc.contact;
        contact_defs.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
        let contacts = contact_defs
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .map(|c| Contact {
                name: c.name,
                callsign: c.callsign.filter(|s| !s.is_empty()),
                issi: c.issi,
                number: c.number.filter(|s| !s.is_empty()),
                gateway: c.gateway.filter(|s| !s.is_empty()),
                order: c.order,
            })
            .collect();

        Some(Codeplug {
            folders,
            scanlists,
            gateways,
            contacts,
            names,
        })
    }

    #[allow(dead_code)] // used in tests and by later screens/logs
    pub fn name_of(&self, gssi: u32) -> String {
        self.names
            .get(&gssi)
            .cloned()
            .unwrap_or_else(|| format!("TG {gssi}"))
    }

    /// A programmed name for this identity, or None if it isn't in the codeplug
    /// (no "TG {id}" fallback). Used for the talker line so an unknown talker
    /// shows its raw id on its own line instead of a "TG {id}" label.
    pub fn known_name(&self, gssi: u32) -> Option<String> {
        self.names.get(&gssi).cloned()
    }

    pub fn gateway_by_id(&self, id: &str) -> Option<&Gateway> {
        self.gateways.iter().find(|g| g.id == id)
    }
}

/// The max length of the TETRA external-subscriber-number IE
/// (ETSI TS 100 392-2 cl. 14.8.20).
pub const MAX_EXTERNAL_DIGITS: usize = 24;

/// Strip whitespace and validate the dial-digit set (`0-9 * # +`). Returns the
/// cleaned digit string, or an error describing why it is unencodable.
pub fn normalize_dial(raw: &str) -> Result<String, String> {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_whitespace() {
            continue;
        }
        if c.is_ascii_digit() || c == '*' || c == '#' || c == '+' {
            out.push(c);
        } else {
            return Err(format!("Invalid dial character '{c}'"));
        }
    }
    if out.is_empty() {
        return Err("Empty dial string".to_string());
    }
    if out.chars().count() > MAX_EXTERNAL_DIGITS {
        return Err(format!("Number exceeds {MAX_EXTERNAL_DIGITS} digits"));
    }
    Ok(out)
}

impl Contact {
    /// True when this is a phone (external) contact rather than an ISSI one.
    pub fn is_phone(&self) -> bool {
        self.number.is_some() || self.gateway.is_some()
    }

    /// Resolve to an on-air call target, applying the gateway prefix. Enforces
    /// the exactly-one-form rule, the dial-digit set, and the 24-digit limit.
    pub fn resolve(&self, cp: &Codeplug) -> Result<CallTarget, String> {
        let has_issi = self.issi.is_some();
        if has_issi && self.is_phone() {
            return Err("Contact has both an ISSI and a phone number".to_string());
        }
        if let Some(issi) = self.issi {
            if issi < 1 || issi > 16_777_215 {
                return Err("ISSI out of range (1..=16777215)".to_string());
            }
            return Ok(CallTarget::Individual(issi));
        }
        let number = self
            .number
            .as_deref()
            .ok_or("Contact has no ISSI or number")?;
        let gw_id = self
            .gateway
            .as_deref()
            .ok_or("Phone contact has no gateway")?;
        let gw = cp
            .gateway_by_id(gw_id)
            .ok_or_else(|| format!("Unknown gateway '{gw_id}'"))?;
        let digits = normalize_dial(&format!("{}{}", gw.prefix, number))?;
        Ok(CallTarget::External {
            gateway_ssi: gw.gateway_issi,
            digits,
        })
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

[[scanlist]]
name = "Alpha"
talkgroups = [101, 300]
order = 0

[[gateway]]
id = "pabx1"
name = "HQ PABX"
kind = "pabx"
gateway_issi = 8000002
prefix = "9"

[[contact]]
name = "Alice"
callsign = "A1"
issi = 1000123
order = 1

[[contact]]
name = "Front Desk"
number = "1234"
gateway = "pabx1"
order = 0
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
        assert_eq!(cp.scanlists.len(), 1);
        assert_eq!(cp.scanlists[0].name, "Alpha");
        assert_eq!(cp.scanlists[0].talkgroups, vec![101, 300]);
    }

    #[test]
    fn parses_and_resolves_contacts_and_gateways() {
        let cp = Codeplug::parse(SAMPLE).expect("codeplug parses");
        assert_eq!(cp.gateways.len(), 1);
        assert_eq!(cp.gateways[0].gateway_issi, 8000002);
        // Contacts sorted by order: Front Desk (0) before Alice (1).
        assert_eq!(cp.contacts.len(), 2);
        assert_eq!(cp.contacts[0].name, "Front Desk");
        assert_eq!(cp.contacts[1].name, "Alice");

        // ISSI contact -> individual.
        assert_eq!(
            cp.contacts[1].resolve(&cp).unwrap(),
            CallTarget::Individual(1000123)
        );
        // Phone contact -> external; gateway prefix "9" prepended to "1234".
        assert_eq!(
            cp.contacts[0].resolve(&cp).unwrap(),
            CallTarget::External { gateway_ssi: 8000002, digits: "91234".to_string() }
        );
    }

    #[test]
    fn dial_validation_rules() {
        assert_eq!(normalize_dial(" 12 34 ").unwrap(), "1234");
        assert_eq!(normalize_dial("+49*99#").unwrap(), "+49*99#");
        assert!(normalize_dial("").is_err());
        assert!(normalize_dial("12ab").is_err());
        assert!(normalize_dial(&"9".repeat(25)).is_err());
    }

    #[test]
    fn rejects_contact_with_both_forms() {
        let toml = r#"
[[contact]]
name = "Bad"
issi = 5
number = "1"
gateway = "gw"
"#;
        let cp = Codeplug::parse(toml).expect("parses");
        assert!(cp.contacts[0].resolve(&cp).is_err());
    }

    #[test]
    fn none_or_empty_codeplug() {
        assert!(Codeplug::parse("config_version = \"0.7\"").is_none());
        assert!(Codeplug::parse("not valid toml {{{").is_none());
    }
}
