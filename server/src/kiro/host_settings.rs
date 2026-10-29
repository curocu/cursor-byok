//! Writes the Kiro endpoint overrides that send model traffic to this gateway.
//!
//! Kiro resolves its runtime (inference) and control-plane endpoints from
//! `codewhisperer.config.{endpoints,krsEndpoints,cpsEndpoints}`. Those keys are
//! not declared in the bundled extension manifest, but the extension reads them
//! straight out of the user settings file, so writing them is enough: no local
//! CA, no proxy interception, and no patching of the extension bundle.
//!
//! Kiro evaluates the endpoints while loading the extension, so a change only
//! takes effect after the window is reloaded.

use std::{collections::BTreeMap, fs, path::PathBuf};

use serde_json::{json, Value};

use crate::{Error, Result};

const CONFIG_SECTION: &str = "codewhisperer.config";

/// Keys the runtime (inference) endpoint is read from.
const RUNTIME_KEYS: [&str; 2] = ["endpoints", "krsEndpoints"];
/// Key the control-plane endpoint is read from.
const CONTROL_KEY: &str = "cpsEndpoints";
/// Every key this module owns.
const KEYS: [&str; 3] = ["endpoints", "krsEndpoints", "cpsEndpoints"];

/// Every region Kiro may resolve. Mapping all of them keeps the takeover
/// working whichever region the account lands on, and satisfies the extension's
/// requirement that the runtime list contain a `us-east-1` entry.
const REGIONS: [&str; 8] = [
    "us-east-1",
    "eu-central-1",
    "us-gov-east-1",
    "us-gov-west-1",
    "us-iso-east-1",
    "us-isob-east-1",
    "us-isof-south-1",
    "us-isof-east-1",
];

fn path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    match std::env::consts::OS {
        "macos" => Ok(home.join("Library/Application Support/Kiro/User/settings.json")),
        "windows" => Ok(std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Kiro/User/settings.json")),
        "linux" => Ok(std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("Kiro/User/settings.json")),
        platform => Err(Error::Config(format!(
            "Kiro settings are unsupported on {platform}"
        ))),
    }
}

fn read() -> Result<BTreeMap<String, Value>> {
    let path = path()?;
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    if data.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    json5::from_str(&data)
        .map_err(|error| Error::Config(format!("parse Kiro settings JSONC: {error}")))
}

fn write(settings: &BTreeMap<String, Value>) -> Result<()> {
    let path = path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(settings)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, [data.as_slice(), b"\n"].concat())?;
    fs::rename(temp, path)?;
    Ok(())
}

/// Points every runtime and control-plane region at the gateway.
pub fn apply(runtime_port: u16, control_port: u16) -> Result<()> {
    let mut settings = read()?;
    for key in RUNTIME_KEYS {
        settings.insert(qualified(key), endpoint_list(runtime_port));
    }
    settings.insert(qualified(CONTROL_KEY), endpoint_list(control_port));
    write(&settings)
}

/// Removes the overrides so Kiro falls back to its built-in endpoints.
pub fn clear() -> Result<()> {
    let mut settings = read()?;
    let before = settings.len();
    for key in KEYS {
        settings.remove(&qualified(key));
    }
    if settings.len() != before {
        write(&settings)?;
    }
    Ok(())
}

/// Reports whether the settings file already points at these ports.
pub fn matches(runtime_port: u16, control_port: u16) -> Result<bool> {
    Ok(overridden(&read()?, runtime_port, control_port))
}

fn qualified(key: &str) -> String {
    format!("{CONFIG_SECTION}.{key}")
}

fn endpoint_list(port: u16) -> Value {
    Value::Array(
        REGIONS
            .iter()
            .map(|region| json!({ "region": region, "endpoint": format!("http://127.0.0.1:{port}") }))
            .collect(),
    )
}

fn overridden(
    settings: &BTreeMap<String, Value>,
    runtime_port: u16,
    control_port: u16,
) -> bool {
    let runtime = endpoint_list(runtime_port);
    let control = endpoint_list(control_port);
    RUNTIME_KEYS
        .iter()
        .all(|key| settings.get(&qualified(key)) == Some(&runtime))
        && settings.get(&qualified(CONTROL_KEY)) == Some(&control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn every_region_is_mapped_to_the_gateway_port() {
        let list = endpoint_list(19800);
        let entries = list.as_array().unwrap();
        assert_eq!(entries.len(), REGIONS.len());
        assert_eq!(entries[0]["region"], json!("us-east-1"));
        assert_eq!(entries[0]["endpoint"], json!("http://127.0.0.1:19800"));
        assert!(entries
            .iter()
            .all(|entry| entry["endpoint"] == json!("http://127.0.0.1:19800")));
    }

    #[test]
    fn the_runtime_list_always_contains_us_east_1() {
        // The extension refuses to start when the list has no us-east-1 entry.
        let list = endpoint_list(19800);
        assert!(list
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["region"] == json!("us-east-1")));
    }

    #[test]
    fn overridden_requires_all_three_keys_at_the_expected_ports() {
        let settings = settings_with(&[
            ("codewhisperer.config.endpoints", endpoint_list(19800)),
            ("codewhisperer.config.krsEndpoints", endpoint_list(19800)),
            ("codewhisperer.config.cpsEndpoints", endpoint_list(19801)),
        ]);
        assert!(overridden(&settings, 19800, 19801));

        // A single key pointing elsewhere is not a complete takeover.
        let partial = settings_with(&[
            ("codewhisperer.config.endpoints", endpoint_list(19800)),
            ("codewhisperer.config.krsEndpoints", endpoint_list(19800)),
        ]);
        assert!(!overridden(&partial, 19800, 19801));

        // Swapping the runtime and control ports is not a match either.
        assert!(!overridden(&settings, 19801, 19800));
    }
}
