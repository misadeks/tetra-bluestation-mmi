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

/// A talkgroup as programmed, with its folder + sort key, for the editor (the
/// grouped `folders` view drops these details).
#[derive(Debug, Clone)]
pub struct TalkgroupMeta {
    pub gssi: u32,
    pub name: String,
    pub folder: Option<String>,
    pub class_of_usage: u8,
    pub order: i64,
}

/// A folder definition (id/name/order), for the editor. Includes folders that
/// currently hold no talkgroups.
#[derive(Debug, Clone)]
pub struct FolderMeta {
    pub id: String,
    pub name: String,
    pub order: i64,
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
    pub active: bool,
    pub order: i64,
}

/// A home network ([net_info]) or an additional allowed network ([[network]]).
#[derive(Debug, Clone)]
pub struct Network {
    /// Mobile Country Code (10-bit, 0..=1023).
    pub mcc: u16,
    /// Mobile Network Code (14-bit, 0..=16383).
    pub mnc: u16,
    pub name: Option<String>,
    pub priority: i64,
    /// True for the home network ([net_info]); it is always allowed.
    pub home: bool,
}

/// The `[codeplug].home_display` feature toggle.
#[derive(Debug, Clone)]
pub struct HomeDisplay {
    pub enabled: bool,
    /// SDS protocol id (0..=255); 130 = 0x82 text messaging SDS-TL.
    pub pid: u8,
}

/// Codeplug-wide scalar settings ([codeplug] table). Growth point for future
/// per-feature sub-tables.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub home_display: Option<HomeDisplay>,
}

/// External-network access point (gateway). A gateway is just an addressing
/// shortcut: dial `gateway_issi` with the (prefixed) number as the called party.
#[derive(Debug, Clone)]
pub struct Gateway {
    pub id: String,
    pub name: String,
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
    pub settings: Settings,
    pub networks: Vec<Network>,
    pub folders: Vec<Folder>,
    /// All folder definitions (including empty ones), for the folder editor.
    pub folder_defs: Vec<FolderMeta>,
    /// Flat talkgroup list (with folder + order), for the talkgroup editor.
    pub all_talkgroups: Vec<TalkgroupMeta>,
    pub scanlists: Vec<Scanlist>,
    pub gateways: Vec<Gateway>,
    pub contacts: Vec<Contact>,
    #[allow(dead_code)] // used by name_of, handy for later screens/logs
    names: HashMap<u32, String>,
}

#[derive(Deserialize)]
struct Doc {
    #[serde(default)]
    net_info: Option<NetInfoDef>,
    #[serde(default)]
    codeplug: Option<CodeplugSettingsDef>,
    #[serde(default)]
    network: Vec<NetworkDef>,
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
struct NetInfoDef {
    #[serde(default)]
    mcc: u16,
    #[serde(default)]
    mnc: u16,
}

#[derive(Deserialize)]
struct NetworkDef {
    #[serde(default)]
    mcc: u16,
    #[serde(default)]
    mnc: u16,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    priority: i64,
}

#[derive(Deserialize)]
struct CodeplugSettingsDef {
    #[serde(default)]
    home_display: Option<HomeDisplayDef>,
}

#[derive(Deserialize)]
struct HomeDisplayDef {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_pid")]
    pid: u8,
}

fn default_pid() -> u8 {
    130
}

#[derive(Deserialize)]
struct GatewayDef {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
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
    #[serde(default = "default_true")]
    active: bool,
    #[serde(default)]
    order: i64,
}

fn default_true() -> bool {
    true
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
    /// unparseable or carries no editable sections.
    pub fn parse(toml_str: &str) -> Option<Codeplug> {
        let doc: Doc = toml::from_str(toml_str).ok()?;
        if doc.talkgroup.is_empty()
            && doc.contact.is_empty()
            && doc.gateway.is_empty()
            && doc.folder.is_empty()
            && doc.scanlist.is_empty()
            && doc.network.is_empty()
            && doc.net_info.is_none()
            && doc.codeplug.is_none()
        {
            return None;
        }

        // Home network ([net_info]) first, then additional [[network]] by priority.
        let mut networks: Vec<Network> = Vec::new();
        if let Some(ni) = &doc.net_info {
            networks.push(Network {
                mcc: ni.mcc,
                mnc: ni.mnc,
                name: None,
                priority: i64::MIN,
                home: true,
            });
        }
        let mut extra: Vec<Network> = doc
            .network
            .iter()
            .map(|n| Network {
                mcc: n.mcc,
                mnc: n.mnc,
                name: n.name.clone().filter(|s| !s.is_empty()),
                priority: n.priority,
                home: false,
            })
            .collect();
        extra.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.mcc.cmp(&b.mcc))
                .then_with(|| a.mnc.cmp(&b.mnc))
        });
        networks.extend(extra);

        // Codeplug-wide settings.
        let settings = Settings {
            home_display: doc.codeplug.as_ref().and_then(|c| {
                c.home_display.as_ref().map(|h| HomeDisplay {
                    enabled: h.enabled,
                    pid: h.pid,
                })
            }),
        };

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

        // Editor-oriented flat lists (all folders incl. empty, all talkgroups).
        let folder_metas: Vec<FolderMeta> = folder_defs
            .iter()
            .filter(|f| !f.id.is_empty())
            .map(|f| FolderMeta {
                id: f.id.clone(),
                name: if f.name.is_empty() { f.id.clone() } else { f.name.clone() },
                order: f.order,
            })
            .collect();
        let mut all_talkgroups: Vec<TalkgroupMeta> = doc
            .talkgroup
            .iter()
            .map(|t| TalkgroupMeta {
                gssi: t.gssi,
                name: names[&t.gssi].clone(),
                folder: t.folder.clone().filter(|s| !s.is_empty()),
                class_of_usage: t.class_of_usage.unwrap_or(0),
                order: t.order,
            })
            .collect();
        all_talkgroups.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));

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
                active: s.active,
                order: s.order,
            })
            .collect();

        let gateways = doc
            .gateway
            .into_iter()
            .filter(|g| !g.id.is_empty())
            .map(|g| Gateway {
                name: if g.name.is_empty() { g.id.clone() } else { g.name },
                id: g.id,
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
            settings,
            networks,
            folders,
            folder_defs: folder_metas,
            all_talkgroups,
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
    pub fn is_phone(&self) -> bool {        self.number.is_some() || self.gateway.is_some()
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

/// Validated input for creating or updating a `[[contact]]`. Exactly one target
/// form must be present: `issi`, or (`number` + `gateway`).
#[derive(Debug, Clone)]
pub struct ContactInput {
    pub name: String,
    pub callsign: Option<String>,
    pub issi: Option<u32>,
    pub number: Option<String>,
    pub gateway: Option<String>,
}

impl ContactInput {
    /// Validate the contact form (used before writing it into the codeplug).
    /// Mirrors the stack-side rules so the UI can reject bad input early.
    pub fn validate(&self, cp: &Codeplug) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name is required".to_string());
        }
        let has_issi = self.issi.is_some();
        let has_phone = self.number.is_some() || self.gateway.is_some();
        if has_issi && has_phone {
            return Err("Choose either an ISSI or a number, not both".to_string());
        }
        if !has_issi && !has_phone {
            return Err("Enter an ISSI or a number".to_string());
        }
        if let Some(issi) = self.issi {
            if issi < 1 || issi > 16_777_215 {
                return Err("ISSI must be 1..=16777215".to_string());
            }
        }
        if has_phone {
            let number = self
                .number
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or("Enter a number to dial")?;
            let gw_id = self.gateway.as_deref().ok_or("Select a gateway")?;
            let gw = cp
                .gateway_by_id(gw_id)
                .ok_or_else(|| format!("Unknown gateway '{gw_id}'"))?;
            normalize_dial(&format!("{}{}", gw.prefix, number))?;
        }
        Ok(())
    }
}

/// Serialize a contact's fields into a fresh toml_edit table (order preserved by
/// the caller). Sets only the fields for the chosen form so stale keys never
/// linger when a contact switches between ISSI and phone forms.
fn contact_table(input: &ContactInput, order: i64) -> toml_edit::Table {
    use toml_edit::value;
    let mut t = toml_edit::Table::new();
    t["name"] = value(input.name.clone());
    if let Some(cs) = input.callsign.as_ref().filter(|s| !s.is_empty()) {
        t["callsign"] = value(cs.clone());
    }
    if let Some(issi) = input.issi {
        t["issi"] = value(issi as i64);
    }
    if let Some(num) = input.number.as_ref().filter(|s| !s.is_empty()) {
        t["number"] = value(num.clone());
    }
    if let Some(gw) = input.gateway.as_ref().filter(|s| !s.is_empty()) {
        t["gateway"] = value(gw.clone());
    }
    t["order"] = value(order);
    t
}

fn contacts_array(doc: &mut toml_edit::DocumentMut) -> Result<&mut toml_edit::ArrayOfTables, String> {
    let item = doc
        .entry("contact")
        .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    item.as_array_of_tables_mut()
        .ok_or_else(|| "codeplug 'contact' is not an array of tables".to_string())
}

fn table_name(t: &toml_edit::Table) -> Option<&str> {
    t.get("name").and_then(|v| v.as_str())
}

/// Add a new contact, or update the existing one named `key_name`, in the full
/// codeplug TOML. Returns the edited TOML. Other sections (and redacted
/// `"********"` secrets) are preserved verbatim by toml_edit.
pub fn upsert_contact(
    toml_str: &str,
    input: &ContactInput,
    key_name: Option<&str>,
) -> Result<String, String> {
    let mut doc = toml_str
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("codeplug parse error: {e}"))?;

    // Reject a duplicate name (unless it's the row we're editing).
    {
        let arr = contacts_array(&mut doc)?;
        let clash = arr.iter().any(|t| {
            table_name(t) == Some(input.name.as_str()) && key_name != Some(input.name.as_str())
        });
        if clash {
            return Err(format!("A contact named '{}' already exists", input.name));
        }
    }

    let arr = contacts_array(&mut doc)?;
    if let Some(key) = key_name {
        let pos = arr
            .iter()
            .position(|t| table_name(t) == Some(key))
            .ok_or_else(|| format!("Contact '{key}' not found"))?;
        let order = arr
            .get(pos)
            .and_then(|t| t.get("order"))
            .and_then(|v| v.as_integer())
            .unwrap_or(pos as i64);
        let table = contact_table(input, order);
        if let Some(slot) = arr.get_mut(pos) {
            *slot = table;
        }
    } else {
        let next_order = arr
            .iter()
            .filter_map(|t| t.get("order").and_then(|v| v.as_integer()))
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        arr.push(contact_table(input, next_order));
    }
    Ok(finish(doc))
}

/// Delete the contact named `name` from the full codeplug TOML.
pub fn delete_contact(toml_str: &str, name: &str) -> Result<String, String> {
    let mut doc = toml_str
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("codeplug parse error: {e}"))?;
    let arr = contacts_array(&mut doc)?;
    let pos = arr
        .iter()
        .position(|t| table_name(t) == Some(name))
        .ok_or_else(|| format!("Contact '{name}' not found"))?;
    arr.remove(pos);
    Ok(finish(doc))
}

// --- Shared write helpers ----------------------------------------------------

/// Drop MS-obsolete/removed keys, then render the document. Called by every
/// write path so an edit never re-emits `[cell_info]` (MS learns cell identity
/// over the air) or a gateway `kind` (removed: TETRA has no PABX/PSTN split).
fn finish(mut doc: toml_edit::DocumentMut) -> String {
    sanitize_doc(&mut doc);
    doc.to_string()
}

fn sanitize_doc(doc: &mut toml_edit::DocumentMut) {
    doc.as_table_mut().remove("cell_info");
    if let Some(arr) = doc.get_mut("gateway").and_then(|i| i.as_array_of_tables_mut()) {
        for t in arr.iter_mut() {
            t.remove("kind");
        }
    }
}

fn parse_doc(toml_str: &str) -> Result<toml_edit::DocumentMut, String> {
    toml_str
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("codeplug parse error: {e}"))
}

fn array_of<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    key: &str,
) -> Result<&'a mut toml_edit::ArrayOfTables, String> {
    let item = doc
        .entry(key)
        .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    item.as_array_of_tables_mut()
        .ok_or_else(|| format!("codeplug '{key}' is not an array of tables"))
}

/// Next `order` value for an array-of-tables (max existing + 1).
fn next_order(arr: &toml_edit::ArrayOfTables) -> i64 {
    arr.iter()
        .filter_map(|t| t.get("order").and_then(|v| v.as_integer()))
        .max()
        .map(|m| m + 1)
        .unwrap_or(0)
}

// --- Networks ([net_info] home + [[network]] additional) ---------------------

/// A network form. Home ([net_info]) uses only mcc/mnc.
#[derive(Debug, Clone)]
pub struct NetworkInput {
    pub mcc: u16,
    pub mnc: u16,
    pub name: Option<String>,
    pub priority: i64,
}

impl NetworkInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.mcc > 1023 {
            return Err("MCC must be 0..=1023 (10-bit)".to_string());
        }
        if self.mnc > 16383 {
            return Err("MNC must be 0..=16383 (14-bit)".to_string());
        }
        Ok(())
    }
}

/// Set the home network ([net_info] mcc/mnc), creating the table if needed.
pub fn set_net_info(toml_str: &str, mcc: u16, mnc: u16) -> Result<String, String> {
    use toml_edit::{value, Item, Table};
    if mcc > 1023 {
        return Err("MCC must be 0..=1023 (10-bit)".to_string());
    }
    if mnc > 16383 {
        return Err("MNC must be 0..=16383 (14-bit)".to_string());
    }
    let mut doc = parse_doc(toml_str)?;
    let entry = doc
        .entry("net_info")
        .or_insert(Item::Table(Table::new()));
    let t = entry
        .as_table_mut()
        .ok_or("codeplug 'net_info' is not a table")?;
    t["mcc"] = value(mcc as i64);
    t["mnc"] = value(mnc as i64);
    Ok(finish(doc))
}

fn network_table(input: &NetworkInput) -> toml_edit::Table {
    use toml_edit::value;
    let mut t = toml_edit::Table::new();
    t["mcc"] = value(input.mcc as i64);
    t["mnc"] = value(input.mnc as i64);
    if let Some(n) = input.name.as_ref().filter(|s| !s.is_empty()) {
        t["name"] = value(n.clone());
    }
    t["priority"] = value(input.priority);
    t
}

/// Add or update an additional allowed network. `index` selects an existing
/// `[[network]]` row to overwrite; None appends a new one.
pub fn upsert_network(
    toml_str: &str,
    input: &NetworkInput,
    index: Option<usize>,
) -> Result<String, String> {
    input.validate()?;
    let mut doc = parse_doc(toml_str)?;
    let arr = array_of(&mut doc, "network")?;
    match index {
        Some(i) => {
            let slot = arr
                .get_mut(i)
                .ok_or_else(|| format!("Network #{i} not found"))?;
            *slot = network_table(input);
        }
        None => arr.push(network_table(input)),
    }
    Ok(finish(doc))
}

/// Delete the additional network at `index` in the `[[network]]` array.
pub fn delete_network(toml_str: &str, index: usize) -> Result<String, String> {
    let mut doc = parse_doc(toml_str)?;
    let arr = array_of(&mut doc, "network")?;
    if index >= arr.len() {
        return Err(format!("Network #{index} not found"));
    }
    arr.remove(index);
    Ok(finish(doc))
}

// --- Codeplug settings ([codeplug.home_display]) -----------------------------

/// Set (or clear) the `[codeplug].home_display` feature toggle.
pub fn set_home_display(toml_str: &str, enabled: bool, pid: u8) -> Result<String, String> {
    use toml_edit::{value, Item, Table};
    let mut doc = parse_doc(toml_str)?;
    let cp = doc
        .entry("codeplug")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("codeplug 'codeplug' is not a table")?;
    cp.set_implicit(false);
    let hd = cp
        .entry("home_display")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("codeplug 'codeplug.home_display' is not a table")?;
    hd["enabled"] = value(enabled);
    hd["pid"] = value(pid as i64);
    Ok(finish(doc))
}

// --- Gateways ([[gateway]]) ---------------------------------------------------

#[derive(Debug, Clone)]
pub struct GatewayInput {
    pub id: String,
    pub name: String,
    pub gateway_issi: u32,
    pub prefix: String,
}

impl GatewayInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("Gateway id is required".to_string());
        }
        if self.gateway_issi < 1 || self.gateway_issi > 16_777_215 {
            return Err("Gateway ISSI must be 1..=16777215".to_string());
        }
        if !self.prefix.is_empty() {
            for c in self.prefix.chars() {
                if !(c.is_ascii_digit() || c == '*' || c == '#' || c == '+') {
                    return Err(format!("Invalid prefix character '{c}'"));
                }
            }
        }
        Ok(())
    }
}

fn gateway_table(input: &GatewayInput) -> toml_edit::Table {
    use toml_edit::value;
    let mut t = toml_edit::Table::new();
    t["id"] = value(input.id.clone());
    t["name"] = value(if input.name.is_empty() { input.id.clone() } else { input.name.clone() });
    t["gateway_issi"] = value(input.gateway_issi as i64);
    if !input.prefix.is_empty() {
        t["prefix"] = value(input.prefix.clone());
    }
    t
}

fn table_str<'a>(t: &'a toml_edit::Table, key: &str) -> Option<&'a str> {
    t.get(key).and_then(|v| v.as_str())
}

/// Add or update a gateway, keyed by unique `id`.
pub fn upsert_gateway(
    toml_str: &str,
    input: &GatewayInput,
    key_id: Option<&str>,
) -> Result<String, String> {
    input.validate()?;
    let mut doc = parse_doc(toml_str)?;
    {
        let arr = array_of(&mut doc, "gateway")?;
        let clash = arr
            .iter()
            .any(|t| table_str(t, "id") == Some(input.id.as_str()) && key_id != Some(input.id.as_str()));
        if clash {
            return Err(format!("A gateway with id '{}' already exists", input.id));
        }
    }
    let arr = array_of(&mut doc, "gateway")?;
    if let Some(key) = key_id {
        let pos = arr
            .iter()
            .position(|t| table_str(t, "id") == Some(key))
            .ok_or_else(|| format!("Gateway '{key}' not found"))?;
        if let Some(slot) = arr.get_mut(pos) {
            *slot = gateway_table(input);
        }
    } else {
        arr.push(gateway_table(input));
    }
    Ok(finish(doc))
}

/// Delete the gateway with id `id`. Rejects if a contact still references it.
pub fn delete_gateway(toml_str: &str, id: &str) -> Result<String, String> {
    let mut doc = parse_doc(toml_str)?;
    if let Some(carr) = doc.get("contact").and_then(|i| i.as_array_of_tables()) {
        if carr.iter().any(|t| table_str(t, "gateway") == Some(id)) {
            return Err("A contact still uses this gateway".to_string());
        }
    }
    let arr = array_of(&mut doc, "gateway")?;
    let pos = arr
        .iter()
        .position(|t| table_str(t, "id") == Some(id))
        .ok_or_else(|| format!("Gateway '{id}' not found"))?;
    arr.remove(pos);
    Ok(finish(doc))
}

// --- Folders ([[folder]]) -----------------------------------------------------

#[derive(Debug, Clone)]
pub struct FolderInput {
    pub id: String,
    pub name: String,
}

impl FolderInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("Folder id is required".to_string());
        }
        Ok(())
    }
}

fn folder_table(input: &FolderInput, order: i64) -> toml_edit::Table {
    use toml_edit::value;
    let mut t = toml_edit::Table::new();
    t["id"] = value(input.id.clone());
    t["name"] = value(if input.name.is_empty() { input.id.clone() } else { input.name.clone() });
    t["order"] = value(order);
    t
}

/// Add or update a folder, keyed by unique `id`.
pub fn upsert_folder(
    toml_str: &str,
    input: &FolderInput,
    key_id: Option<&str>,
) -> Result<String, String> {
    input.validate()?;
    let mut doc = parse_doc(toml_str)?;
    {
        let arr = array_of(&mut doc, "folder")?;
        let clash = arr
            .iter()
            .any(|t| table_str(t, "id") == Some(input.id.as_str()) && key_id != Some(input.id.as_str()));
        if clash {
            return Err(format!("A folder with id '{}' already exists", input.id));
        }
    }
    let arr = array_of(&mut doc, "folder")?;
    if let Some(key) = key_id {
        let pos = arr
            .iter()
            .position(|t| table_str(t, "id") == Some(key))
            .ok_or_else(|| format!("Folder '{key}' not found"))?;
        let order = arr
            .get(pos)
            .and_then(|t| t.get("order"))
            .and_then(|v| v.as_integer())
            .unwrap_or(pos as i64);
        if let Some(slot) = arr.get_mut(pos) {
            *slot = folder_table(input, order);
        }
    } else {
        let order = next_order(arr);
        arr.push(folder_table(input, order));
    }
    Ok(finish(doc))
}

/// Delete a folder by id. Talkgroups referencing it fall back to "Other".
pub fn delete_folder(toml_str: &str, id: &str) -> Result<String, String> {
    let mut doc = parse_doc(toml_str)?;
    let arr = array_of(&mut doc, "folder")?;
    let pos = arr
        .iter()
        .position(|t| table_str(t, "id") == Some(id))
        .ok_or_else(|| format!("Folder '{id}' not found"))?;
    arr.remove(pos);
    Ok(finish(doc))
}

// --- Talkgroups ([[talkgroup]]) ----------------------------------------------

#[derive(Debug, Clone)]
pub struct TalkgroupInput {
    pub gssi: u32,
    pub name: String,
    pub folder: Option<String>,
    pub class_of_usage: u8,
}

impl TalkgroupInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.gssi < 1 || self.gssi > 16_777_215 {
            return Err("GSSI must be 1..=16777215".to_string());
        }
        if self.class_of_usage > 7 {
            return Err("Class of usage must be 0..=7".to_string());
        }
        Ok(())
    }
}

fn talkgroup_table(input: &TalkgroupInput, order: i64) -> toml_edit::Table {
    use toml_edit::value;
    let mut t = toml_edit::Table::new();
    t["gssi"] = value(input.gssi as i64);
    t["name"] = value(input.name.clone());
    if let Some(f) = input.folder.as_ref().filter(|s| !s.is_empty()) {
        t["folder"] = value(f.clone());
    }
    t["class_of_usage"] = value(input.class_of_usage as i64);
    t["order"] = value(order);
    t
}

fn table_int(t: &toml_edit::Table, key: &str) -> Option<i64> {
    t.get(key).and_then(|v| v.as_integer())
}

/// Add or update a talkgroup, keyed by unique `gssi`.
pub fn upsert_talkgroup(
    toml_str: &str,
    input: &TalkgroupInput,
    key_gssi: Option<u32>,
) -> Result<String, String> {
    input.validate()?;
    let mut doc = parse_doc(toml_str)?;
    let key_i = key_gssi.map(|g| g as i64);
    {
        let arr = array_of(&mut doc, "talkgroup")?;
        let clash = arr
            .iter()
            .any(|t| table_int(t, "gssi") == Some(input.gssi as i64) && key_i != Some(input.gssi as i64));
        if clash {
            return Err(format!("A talkgroup with GSSI {} already exists", input.gssi));
        }
    }
    let arr = array_of(&mut doc, "talkgroup")?;
    if let Some(key) = key_i {
        let pos = arr
            .iter()
            .position(|t| table_int(t, "gssi") == Some(key))
            .ok_or_else(|| format!("Talkgroup {key} not found"))?;
        let order = table_int(arr.get(pos).unwrap(), "order").unwrap_or(pos as i64);
        if let Some(slot) = arr.get_mut(pos) {
            *slot = talkgroup_table(input, order);
        }
    } else {
        let order = next_order(arr);
        arr.push(talkgroup_table(input, order));
    }
    Ok(finish(doc))
}

/// Delete a talkgroup by GSSI (also removes it from any scanlist membership).
pub fn delete_talkgroup(toml_str: &str, gssi: u32) -> Result<String, String> {
    let mut doc = parse_doc(toml_str)?;
    {
        let arr = array_of(&mut doc, "talkgroup")?;
        let pos = arr
            .iter()
            .position(|t| table_int(t, "gssi") == Some(gssi as i64))
            .ok_or_else(|| format!("Talkgroup {gssi} not found"))?;
        arr.remove(pos);
    }
    // Prune it from scanlist membership arrays.
    if let Some(sl) = doc.get_mut("scanlist").and_then(|i| i.as_array_of_tables_mut()) {
        for t in sl.iter_mut() {
            if let Some(arr) = t.get_mut("talkgroups").and_then(|v| v.as_array_mut()) {
                arr.retain(|v| v.as_integer() != Some(gssi as i64));
            }
        }
    }
    Ok(finish(doc))
}

// --- Scanlists ([[scanlist]]) -------------------------------------------------

#[derive(Debug, Clone)]
pub struct ScanlistInput {
    pub name: String,
    pub talkgroups: Vec<u32>,
    pub active: bool,
}

impl ScanlistInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Scanlist name is required".to_string());
        }
        Ok(())
    }
}

fn scanlist_table(input: &ScanlistInput, order: i64) -> toml_edit::Table {
    use toml_edit::{value, Array};
    let mut t = toml_edit::Table::new();
    t["name"] = value(input.name.clone());
    let mut arr = Array::new();
    for g in &input.talkgroups {
        arr.push(*g as i64);
    }
    t["talkgroups"] = value(arr);
    t["active"] = value(input.active);
    t["order"] = value(order);
    t
}

/// Add or update a scanlist, keyed by unique `name`.
pub fn upsert_scanlist(
    toml_str: &str,
    input: &ScanlistInput,
    key_name: Option<&str>,
) -> Result<String, String> {
    input.validate()?;
    let mut doc = parse_doc(toml_str)?;
    {
        let arr = array_of(&mut doc, "scanlist")?;
        let clash = arr
            .iter()
            .any(|t| table_name(t) == Some(input.name.as_str()) && key_name != Some(input.name.as_str()));
        if clash {
            return Err(format!("A scanlist named '{}' already exists", input.name));
        }
    }
    let arr = array_of(&mut doc, "scanlist")?;
    if let Some(key) = key_name {
        let pos = arr
            .iter()
            .position(|t| table_name(t) == Some(key))
            .ok_or_else(|| format!("Scanlist '{key}' not found"))?;
        let order = table_int(arr.get(pos).unwrap(), "order").unwrap_or(pos as i64);
        if let Some(slot) = arr.get_mut(pos) {
            *slot = scanlist_table(input, order);
        }
    } else {
        let order = next_order(arr);
        arr.push(scanlist_table(input, order));
    }
    Ok(finish(doc))
}

/// Delete a scanlist by name.
pub fn delete_scanlist(toml_str: &str, name: &str) -> Result<String, String> {
    let mut doc = parse_doc(toml_str)?;
    let arr = array_of(&mut doc, "scanlist")?;
    let pos = arr
        .iter()
        .position(|t| table_name(t) == Some(name))
        .ok_or_else(|| format!("Scanlist '{name}' not found"))?;
    arr.remove(pos);
    Ok(finish(doc))
}


#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
config_version = "0.7"

[net_info]
mcc = 901
mnc = 9999

[cell_info]
colour_code = 7
location_area = 1

[codeplug.home_display]
enabled = true
pid = 130

[[network]]
mcc = 262
mnc = 1
name = "Roam DE"
priority = 5

[[folder]]
id    = "work"
name  = "Work"
order = 0

[[folder]]
id    = "ops"
name  = "Operations"
order = 1

[[folder]]
id    = "empty"
name  = "Empty"
order = 2

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
    fn parses_networks_and_settings() {
        let cp = Codeplug::parse(SAMPLE).expect("codeplug parses");
        // Home network first, then the additional one.
        assert_eq!(cp.networks.len(), 2);
        assert!(cp.networks[0].home);
        assert_eq!((cp.networks[0].mcc, cp.networks[0].mnc), (901, 9999));
        assert!(!cp.networks[1].home);
        assert_eq!(cp.networks[1].name.as_deref(), Some("Roam DE"));
        // Home-mode display setting.
        let hd = cp.settings.home_display.expect("home_display present");
        assert!(hd.enabled);
        assert_eq!(hd.pid, 130);
        // Folder defs include the empty folder; gateway carries no kind field.
        assert!(cp.folder_defs.iter().any(|f| f.id == "empty"));
        assert_eq!(cp.all_talkgroups.len(), 4);
    }

    #[test]
    fn write_paths_drop_cell_info_and_gateway_kind() {
        // Editing anything must strip [cell_info] and gateway kind on save.
        let out = delete_scanlist(SAMPLE, "Alpha").expect("edit");
        assert!(!out.contains("[cell_info]"));
        assert!(!out.contains("kind"));
        // Round-trip still parses and keeps the rest.
        let cp = Codeplug::parse(&out).expect("parses");
        assert_eq!(cp.gateways.len(), 1);
        assert!(cp.scanlists.is_empty());
    }

    #[test]
    fn gateway_crud() {
        let input = GatewayInput {
            id: "pstn".to_string(),
            name: "City PSTN".to_string(),
            gateway_issi: 8000005,
            prefix: "0".to_string(),
        };
        let out = upsert_gateway(SAMPLE, &input, None).expect("adds");
        let cp = Codeplug::parse(&out).expect("parses");
        assert!(cp.gateways.iter().any(|g| g.id == "pstn" && g.gateway_issi == 8000005));
        // Cannot delete a gateway a contact references.
        assert!(delete_gateway(&out, "pabx1").is_err());
        // Unused gateway deletes fine.
        let out2 = delete_gateway(&out, "pstn").expect("deletes");
        assert!(!Codeplug::parse(&out2).unwrap().gateways.iter().any(|g| g.id == "pstn"));
    }

    #[test]
    fn folder_talkgroup_scanlist_network_settings_writers() {
        // Folder.
        let out = upsert_folder(SAMPLE, &FolderInput { id: "car".into(), name: "Cars".into() }, None).unwrap();
        assert!(Codeplug::parse(&out).unwrap().folder_defs.iter().any(|f| f.id == "car"));
        // Talkgroup.
        let out = upsert_talkgroup(&out, &TalkgroupInput { gssi: 500, name: "Net5".into(), folder: Some("car".into()), class_of_usage: 2 }, None).unwrap();
        assert!(Codeplug::parse(&out).unwrap().all_talkgroups.iter().any(|t| t.gssi == 500));
        // Deleting a talkgroup prunes scanlist membership.
        let out = delete_talkgroup(&out, 101).unwrap();
        let cp = Codeplug::parse(&out).unwrap();
        assert!(!cp.scanlists[0].talkgroups.contains(&101));
        // Scanlist upsert.
        let out = upsert_scanlist(&out, &ScanlistInput { name: "Beta".into(), talkgroups: vec![300], active: false }, None).unwrap();
        let cp = Codeplug::parse(&out).unwrap();
        assert!(cp.scanlists.iter().any(|s| s.name == "Beta" && !s.active));
        // Network + net_info + settings.
        let out = set_net_info(&out, 262, 2).unwrap();
        assert_eq!(Codeplug::parse(&out).unwrap().networks[0].mnc, 2);
        let out = upsert_network(&out, &NetworkInput { mcc: 310, mnc: 260, name: Some("US".into()), priority: 1 }, None).unwrap();
        assert!(Codeplug::parse(&out).unwrap().networks.iter().any(|n| n.mcc == 310));
        let out = set_home_display(&out, false, 200).unwrap();
        let hd = Codeplug::parse(&out).unwrap().settings.home_display.unwrap();
        assert!(!hd.enabled && hd.pid == 200);
        // MCC/MNC range validation.
        assert!(set_net_info(SAMPLE, 2000, 1).is_err());
    }

    #[test]
    fn parses_folders_and_talkgroups() {
        let cp = Codeplug::parse(SAMPLE).expect("codeplug parses");
        // Work, Operations, Other (the empty folder has no talkgroups).
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
    fn upsert_adds_and_updates_contacts() {
        // Add a new ISSI contact to the sample.
        let input = ContactInput {
            name: "Bravo".to_string(),
            callsign: Some("B2".to_string()),
            issi: Some(2000),
            number: None,
            gateway: None,
        };
        let toml2 = upsert_contact(SAMPLE, &input, None).expect("adds");
        let cp2 = Codeplug::parse(&toml2).expect("parses");
        assert!(cp2.contacts.iter().any(|c| c.name == "Bravo" && c.issi == Some(2000)));

        // Update Alice: switch her to a phone contact (issi key must be dropped).
        let upd = ContactInput {
            name: "Alice".to_string(),
            callsign: None,
            issi: None,
            number: Some("555".to_string()),
            gateway: Some("pabx1".to_string()),
        };
        let toml3 = upsert_contact(&toml2, &upd, Some("Alice")).expect("updates");
        let cp3 = Codeplug::parse(&toml3).expect("parses");
        let alice = cp3.contacts.iter().find(|c| c.name == "Alice").unwrap();
        assert_eq!(alice.issi, None);
        assert_eq!(alice.number.as_deref(), Some("555"));
        assert_eq!(alice.gateway.as_deref(), Some("pabx1"));

        // Duplicate name is rejected.
        let dup = ContactInput {
            name: "Alice".to_string(),
            callsign: None,
            issi: Some(7),
            number: None,
            gateway: None,
        };
        assert!(upsert_contact(&toml3, &dup, None).is_err());
    }

    #[test]
    fn delete_removes_contact() {
        let toml2 = delete_contact(SAMPLE, "Alice").expect("deletes");
        let cp2 = Codeplug::parse(&toml2).expect("parses");
        assert!(!cp2.contacts.iter().any(|c| c.name == "Alice"));
        // Other data (talkgroups, gateways) survives.
        assert!(!cp2.folders.is_empty());
        assert_eq!(cp2.gateways.len(), 1);
        assert!(delete_contact(&toml2, "Nobody").is_err());
    }

    #[test]
    fn none_or_empty_codeplug() {
        assert!(Codeplug::parse("config_version = \"0.7\"").is_none());
        assert!(Codeplug::parse("not valid toml {{{").is_none());
    }
}
