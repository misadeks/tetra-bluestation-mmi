// Serde-only mirror of the BlueStation MS external interface (interface-2).
//
// This is Option 3 from the kickoff: a lean, standalone protocol layer with no
// dependency on the stack repo. Outbound commands are built as serde_json
// Values in the exact externally-tagged shape the wire uses; inbound frames are
// parsed as Values and, where we care, into typed structs (MsRuntimeState).
//
// Fidelity notes (verified against the stack types and PROTOCOL.md):
// - Enums are serde externally-tagged: {"Variant": { ...fields... }}.
// - No serde renames; field and variant names are exact Rust identifiers.
// - Optional fields may be omitted; unknown inbound variants are tolerated.
// - Frames are JSON (serde_json) inside binary WebSocket frames.

use serde::Deserialize;
use serde_json::{json, Value};

// Subprotocols the stack requests per channel; the server must accept/echo them.
pub const CONTROL_SUBPROTOCOL: &str = "bluestation-control-v1";
pub const TELEMETRY_SUBPROTOCOL: &str = "bluestation-telemetry-v1";

// Interface schema this app implements (returned/expected for GetInterfaceVersion).
#[allow(dead_code)] // used when we validate/report the version in a later milestone
pub const INTERFACE_VERSION: &str = "bluestation-ms-interface-2";

// --- Outbound command builders (UI -> stack), externally-tagged --------------

fn management(inner: Value) -> Value {
    json!({ "Management": inner })
}

pub fn get_state(handle: u32) -> Value {
    management(json!({ "GetState": { "handle": handle } }))
}

pub fn get_interface_version(handle: u32) -> Value {
    management(json!({ "GetInterfaceVersion": { "handle": handle } }))
}

pub fn get_config(handle: u32) -> Value {
    management(json!({ "GetConfig": { "handle": handle } }))
}

#[allow(dead_code)] // codeplug SetConfig lands in a later milestone (M7)
pub fn set_config(handle: u32, toml: &str) -> Value {
    management(json!({ "SetConfig": { "handle": handle, "toml": toml } }))
}

#[allow(dead_code)] // codeplug apply lands in a later milestone (M7)
pub fn apply_config(handle: u32) -> Value {
    management(json!({ "ApplyConfig": { "handle": handle } }))
}

/// TnmmRegistration (Plane A). Identity (issi, mcc/mnc) is owned by the MS and
/// read from MsRuntimeState; it is never configured. `registration_type` is one
/// of "PeriodicRegistration" | "RegistrationToIndicatedCell".
pub fn tnmm_registration(
    handle: u32,
    registration_type: &str,
    issi: u32,
    mcc_of_issi: u16,
    mnc_of_issi: u16,
) -> Value {
    json!({ "TnmmRegistration": { "handle": handle, "request": {
        "registration_type": registration_type,
        "issi": issi,
        "mcc_of_issi": mcc_of_issi,
        "mnc_of_issi": mnc_of_issi,
    }}})
}

/// TnmmDeregistration (Plane A). All request fields are optional.
pub fn tnmm_deregistration(handle: u32, issi: Option<u32>, mcc: Option<u16>, mnc: Option<u16>) -> Value {
    let mut request = serde_json::Map::new();
    if let Some(v) = issi {
        request.insert("issi".into(), json!(v));
    }
    if let Some(v) = mcc {
        request.insert("mcc".into(), json!(v));
    }
    if let Some(v) = mnc {
        request.insert("mnc".into(), json!(v));
    }
    json!({ "TnmmDeregistration": { "handle": handle, "request": Value::Object(request) } })
}

/// Map an on-air class of usage (0..7) to the TNMM enum string
/// (ClassOfUsage(N+1); ClassOfUsage1 == on-air 0).
pub fn class_of_usage(onair: u8) -> String {
    format!("ClassOfUsage{}", onair.min(7) as u16 + 1)
}

/// Switch the TX talkgroup: detach the currently active group identities and
/// attach `gssi` in one PDU (TnmmAttachDetachGroupIdentity, "select"). `gtsi`
/// carries the plain GSSI (the stack uses the low 24 bits).
pub fn tnmm_switch_talkgroup(handle: u32, gssi: u32, cou_onair: u8) -> Value {
    json!({ "TnmmAttachDetachGroupIdentity": { "handle": handle, "request": {
        "group_identity_attach_detach_mode": "DetachTheCurrentlyActiveGroupIdentities",
        "group_identity_request": [{
            "gtsi": gssi,
            "group_identity_attach_detach_type_identifier": "Attachment",
            "class_of_usage": class_of_usage(cou_onair),
            "group_identity_detachment_request": null,
        }],
        "group_identity_report": "ReportNotRequested",
    }}})
}

// --- Inbound parsing helpers -------------------------------------------------

/// Return (variant_name, payload) for an externally-tagged message object.
pub fn variant_of(message: &Value) -> Option<(&str, &Value)> {
    let obj = message.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    obj.iter().next().map(|(k, v)| (k.as_str(), v))
}

// --- Typed runtime state -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum RegistrationState {
    #[default]
    Idle,
    Registering,
    Registered,
    Detaching,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum ServiceStatus {
    InService,
    InGracefulServiceDegradationMode,
    InServiceWaitingForRegistration,
    #[default]
    OutOfService,
    MmBusy,
    MmIdle,
    #[serde(other)]
    Unknown,
}

/// MsRuntimeState from Management::State. Fields default so a partial state still
/// parses (the stack sends them all, but we stay robust). interface-2 also adds
/// `active_scanlists` / `selection_mode_manual`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MsRuntimeState {
    pub registration_state: RegistrationState,
    pub service_status: ServiceStatus,
    pub own_issi: u32,
    pub home_mcc: u16,
    pub home_mnc: u16,
    pub serving_la: u16,
    pub rssi_dbfs: Option<f32>,
    pub colour_code: u8,
    pub attached_groups: Vec<u32>,
    pub restart_required: bool,
    pub active_scanlists: Option<Vec<String>>,
    pub selection_mode_manual: Option<bool>,
}

impl RegistrationState {
    pub fn label(self) -> &'static str {
        match self {
            RegistrationState::Idle => "Idle",
            RegistrationState::Registering => "Registering",
            RegistrationState::Registered => "Registered",
            RegistrationState::Detaching => "Detaching",
            RegistrationState::Unknown => "Unknown",
        }
    }
}

impl ServiceStatus {
    pub fn label(self) -> &'static str {
        match self {
            ServiceStatus::InService => "In service",
            ServiceStatus::InGracefulServiceDegradationMode => "Degraded",
            ServiceStatus::InServiceWaitingForRegistration => "Waiting for registration",
            ServiceStatus::OutOfService => "Out of service",
            ServiceStatus::MmBusy => "Busy",
            ServiceStatus::MmIdle => "Idle",
            ServiceStatus::Unknown => "Unknown",
        }
    }

    pub fn in_service(self) -> bool {
        matches!(
            self,
            ServiceStatus::InService | ServiceStatus::InGracefulServiceDegradationMode
        )
    }
}

/// Map an uncalibrated serving-cell dBFS level to 0..=5 signal bars. `None`
/// (no measurement yet / out of service) is 0 bars. Thresholds are approximate
/// and tunable; dBFS here is negative, closer to 0 is stronger.
pub fn rssi_to_bars(rssi_dbfs: Option<f32>) -> i32 {
    match rssi_dbfs {
        None => 0,
        Some(v) if v >= -45.0 => 5,
        Some(v) if v >= -60.0 => 4,
        Some(v) if v >= -75.0 => 3,
        Some(v) if v >= -90.0 => 2,
        Some(v) if v >= -105.0 => 1,
        Some(_) => 0,
    }
}

/// Telemetry variants after which MsRuntimeState is expected to have changed, so
/// we pull a fresh GetState immediately (mirrors app/stack_servers.py).
pub fn is_state_changing_event(variant: &str) -> bool {
    matches!(
        variant,
        "TnmmAttachDetachGroupIdentityConfirm"
            | "MsGroupAttach"
            | "MsGroupDetach"
            | "TnmmRegistrationConfirm"
            | "TnmmRegistrationIndication"
            | "TnmmDeregistrationConfirm"
            | "MsRegistrationChange"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_commands_have_exact_wire_shape() {
        assert_eq!(
            get_state(1),
            json!({"Management": {"GetState": {"handle": 1}}})
        );
        assert_eq!(
            get_interface_version(2),
            json!({"Management": {"GetInterfaceVersion": {"handle": 2}}})
        );
        assert_eq!(
            get_config(3),
            json!({"Management": {"GetConfig": {"handle": 3}}})
        );
    }

    #[test]
    fn tnmm_registration_shape() {
        let v = tnmm_registration(6, "RegistrationToIndicatedCell", 1000001, 901, 9999);
        assert_eq!(
            v,
            json!({"TnmmRegistration": {"handle": 6, "request": {
                "registration_type": "RegistrationToIndicatedCell",
                "issi": 1000001, "mcc_of_issi": 901, "mnc_of_issi": 9999
            }}})
        );
    }

    #[test]
    fn deregistration_omits_absent_fields() {
        let v = tnmm_deregistration(7, Some(1000001), None, None);
        assert_eq!(
            v,
            json!({"TnmmDeregistration": {"handle": 7, "request": {"issi": 1000001}}})
        );
    }

    #[test]
    fn parses_management_state_response() {
        let frame = json!({"Management": {"State": {"handle": 1, "state": {
            "registration_state": "Registered",
            "service_status": "InService",
            "own_issi": 1000001,
            "home_mcc": 901,
            "home_mnc": 9999,
            "serving_la": 4,
            "rssi_dbfs": -58.0,
            "colour_code": 7,
            "attached_groups": [100, 200],
            "restart_required": false
        }}}});
        let (variant, payload) = variant_of(&frame).unwrap();
        assert_eq!(variant, "Management");
        let (inner, mgmt) = variant_of(payload).unwrap();
        assert_eq!(inner, "State");
        let state: MsRuntimeState = serde_json::from_value(mgmt["state"].clone()).unwrap();
        assert_eq!(state.registration_state, RegistrationState::Registered);
        assert_eq!(state.service_status, ServiceStatus::InService);
        assert_eq!(state.own_issi, 1000001);
        assert_eq!(state.attached_groups, vec![100, 200]);
        assert_eq!(rssi_to_bars(state.rssi_dbfs), 4);
    }

    #[test]
    fn unknown_enum_values_do_not_fail() {
        let state: MsRuntimeState = serde_json::from_value(json!({
            "registration_state": "SomethingNew",
            "service_status": "AlsoNew"
        }))
        .unwrap();
        assert_eq!(state.registration_state, RegistrationState::Unknown);
        assert_eq!(state.service_status, ServiceStatus::Unknown);
    }

    #[test]
    fn switch_talkgroup_shape() {
        assert_eq!(class_of_usage(0), "ClassOfUsage1");
        assert_eq!(class_of_usage(3), "ClassOfUsage4");
        let v = tnmm_switch_talkgroup(9, 101, 0);
        assert_eq!(
            v,
            json!({"TnmmAttachDetachGroupIdentity": {"handle": 9, "request": {
                "group_identity_attach_detach_mode": "DetachTheCurrentlyActiveGroupIdentities",
                "group_identity_request": [{
                    "gtsi": 101,
                    "group_identity_attach_detach_type_identifier": "Attachment",
                    "class_of_usage": "ClassOfUsage1",
                    "group_identity_detachment_request": null
                }],
                "group_identity_report": "ReportNotRequested"
            }}})
        );
    }

    #[test]
    fn telemetry_variant_extraction_and_state_change() {
        let ev = json!({"MsGroupAttach": {"issi": 1000001, "gssis": [100, 200]}});
        let (variant, _payload) = variant_of(&ev).unwrap();
        assert_eq!(variant, "MsGroupAttach");
        assert!(is_state_changing_event(variant));
        assert!(!is_state_changing_event("MsSpeechFrame"));
    }
}
