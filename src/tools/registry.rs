//! `Registry` tool: read/write the Windows Registry via direct API calls
//! (rather than shelling out to PowerShell).
//!
//! Paths use PowerShell-style root prefixes (e.g. `HKCU:\Software\MyApp`).

use rmcp::schemars;
use serde::Deserialize;
use windows_registry::{Key, Type, Value};

/// Parameters for the `Registry` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RegistryParams {
    /// Operation to perform.
    #[schemars(description = "Operation: get, set, delete, list.")]
    pub mode: RegistryMode,
    /// Registry path in PowerShell format, e.g. `HKCU:\Software\MyApp`.
    #[schemars(description = "Registry path in PowerShell format, e.g. HKCU:\\Software\\MyApp.")]
    pub path: String,
    /// Value name. Required for get; omit on delete to remove the whole key.
    pub name: Option<String>,
    /// Value data. Required for set.
    pub value: Option<String>,
    /// Registry value type for set mode.
    #[serde(default = "default_reg_type", rename = "type")]
    #[schemars(
        description = "Value type for set mode: String, ExpandString, Binary, DWord, MultiString, QWord."
    )]
    pub value_type: RegistryValueType,
}

fn default_reg_type() -> RegistryValueType {
    RegistryValueType::String
}

/// `Registry` operation mode.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegistryMode {
    Get,
    Set,
    Delete,
    List,
}

/// Registry value type, matching PowerShell's `Set-ItemProperty -Type` values.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
pub enum RegistryValueType {
    String,
    ExpandString,
    Binary,
    DWord,
    MultiString,
    QWord,
}

impl std::fmt::Display for RegistryValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RegistryValueType::String => "String",
            RegistryValueType::ExpandString => "ExpandString",
            RegistryValueType::Binary => "Binary",
            RegistryValueType::DWord => "DWord",
            RegistryValueType::MultiString => "MultiString",
            RegistryValueType::QWord => "QWord",
        };
        f.write_str(s)
    }
}

/// Runs the `Registry` tool and returns a caller-facing text response.
pub fn registry(params: RegistryParams) -> String {
    match params.mode {
        RegistryMode::Get => match params.name {
            None => "Error: name parameter is required for get mode.".to_string(),
            Some(name) => get_value(&params.path, &name),
        },
        RegistryMode::Set => match (params.name, params.value) {
            (None, _) => "Error: name parameter is required for set mode.".to_string(),
            (_, None) => "Error: value parameter is required for set mode.".to_string(),
            (Some(name), Some(value)) => set_value(&params.path, &name, &value, params.value_type),
        },
        RegistryMode::Delete => delete_entry(&params.path, params.name.as_deref()),
        RegistryMode::List => list_key(&params.path),
    }
}

/// Splits a PowerShell-style registry path (`HKCU:\Software\MyApp`) into its
/// root hive and subkey path (`Software\MyApp`).
fn parse_registry_path(path: &str) -> Result<(&'static Key, String), String> {
    let (root_str, rest) = path
        .split_once(':')
        .ok_or_else(|| format!("Invalid registry path: {path}"))?;
    let root: &'static Key = match root_str.to_ascii_uppercase().as_str() {
        "HKCU" | "HKEY_CURRENT_USER" => windows_registry::CURRENT_USER,
        "HKLM" | "HKEY_LOCAL_MACHINE" => windows_registry::LOCAL_MACHINE,
        "HKCR" | "HKEY_CLASSES_ROOT" => windows_registry::CLASSES_ROOT,
        "HKU" | "HKEY_USERS" => windows_registry::USERS,
        "HKCC" | "HKEY_CURRENT_CONFIG" => windows_registry::CURRENT_CONFIG,
        other => return Err(format!("Unknown registry root \"{other}\".")),
    };
    let subpath = rest.trim_start_matches('\\').to_string();
    Ok((root, subpath))
}

fn get_value(path: &str, name: &str) -> String {
    let (root, subpath) = match parse_registry_path(path) {
        Ok(v) => v,
        Err(e) => return format!("Error reading registry: {e}"),
    };
    let key = match root.open(&subpath) {
        Ok(k) => k,
        Err(e) => return format!("Error reading registry: {e}"),
    };
    let value = match key.get_value(name) {
        Ok(v) => v,
        Err(e) => return format!("Error reading registry: {e}"),
    };
    format!(
        "Registry value [{path}] \"{name}\" = {}",
        format_value(value)
    )
}

fn format_value(value: Value) -> String {
    match value.ty() {
        Type::U32 => u32::try_from(value)
            .map(|n| n.to_string())
            .unwrap_or_default(),
        Type::U64 => u64::try_from(value)
            .map(|n| n.to_string())
            .unwrap_or_default(),
        Type::String | Type::ExpandString => String::try_from(value).unwrap_or_default(),
        Type::MultiString => Vec::<String>::try_from(value)
            .unwrap_or_default()
            .join("\n"),
        Type::Bytes => value
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(" "),
        Type::Other(_) => format!("{:?}", &*value),
    }
}

fn set_value(path: &str, name: &str, value: &str, reg_type: RegistryValueType) -> String {
    let (root, subpath) = match parse_registry_path(path) {
        Ok(v) => v,
        Err(e) => return format!("Error writing registry: {e}"),
    };
    let key = match root.create(&subpath) {
        Ok(k) => k,
        Err(e) => return format!("Error writing registry: {e}"),
    };

    let result = match reg_type {
        RegistryValueType::String => key.set_string(name, value),
        RegistryValueType::ExpandString => key.set_expand_string(name, value),
        RegistryValueType::DWord => match value.parse::<u32>() {
            Ok(n) => key.set_u32(name, n),
            Err(e) => return format!("Error: Invalid DWord value \"{value}\": {e}"),
        },
        RegistryValueType::QWord => match value.parse::<u64>() {
            Ok(n) => key.set_u64(name, n),
            Err(e) => return format!("Error: Invalid QWord value \"{value}\": {e}"),
        },
        RegistryValueType::Binary => match parse_bytes(value) {
            Ok(bytes) => key.set_bytes(name, Type::Bytes, &bytes),
            Err(e) => return format!("Error: Invalid Binary value \"{value}\": {e}"),
        },
        RegistryValueType::MultiString => {
            let items: Vec<String> = value.split('\n').map(|s| s.to_string()).collect();
            key.set_multi_string(name.to_string(), &items)
        }
    };

    match result {
        Ok(()) => {
            format!("Registry value [{path}] \"{name}\" set to \"{value}\" (type: {reg_type}).")
        }
        Err(e) => format!("Error writing registry: {e}"),
    }
}

/// Parses a comma/whitespace separated list of byte tokens (decimal, or
/// hex with a `0x` prefix) into raw bytes for a Binary registry value.
fn parse_bytes(value: &str) -> Result<Vec<u8>, String> {
    value
        .split([',', ' '])
        .filter(|t| !t.is_empty())
        .map(|token| {
            let token = token.trim();
            let parsed = if let Some(hex) = token
                .strip_prefix("0x")
                .or_else(|| token.strip_prefix("0X"))
            {
                u8::from_str_radix(hex, 16)
            } else {
                token.parse::<u8>()
            };
            parsed.map_err(|e| format!("{token:?}: {e}"))
        })
        .collect()
}

fn delete_entry(path: &str, name: Option<&str>) -> String {
    let (root, subpath) = match parse_registry_path(path) {
        Ok(v) => v,
        Err(e) => return format!("Error deleting registry value: {e}"),
    };

    match name {
        Some(name) => {
            let key = match root.options().read().write().open(&subpath) {
                Ok(k) => k,
                Err(e) => return format!("Error deleting registry value: {e}"),
            };
            match key.remove_value(name) {
                Ok(()) => format!("Registry value [{path}] \"{name}\" deleted."),
                Err(e) => format!("Error deleting registry value: {e}"),
            }
        }
        None => {
            if subpath.is_empty() {
                return "Error deleting registry key: refusing to delete a registry hive root."
                    .to_string();
            }
            let (parent_subpath, leaf) = match subpath.rsplit_once('\\') {
                Some((parent, leaf)) => (parent.to_string(), leaf.to_string()),
                None => (String::new(), subpath.clone()),
            };
            let parent_key = match root.options().read().write().open(&parent_subpath) {
                Ok(k) => k,
                Err(e) => return format!("Error deleting registry key: {e}"),
            };
            match parent_key.remove_tree(&leaf) {
                Ok(()) => format!("Registry key [{path}] deleted."),
                Err(e) => format!("Error deleting registry key: {e}"),
            }
        }
    }
}

fn list_key(path: &str) -> String {
    let (root, subpath) = match parse_registry_path(path) {
        Ok(v) => v,
        Err(e) => return format!("Error listing registry: {e}"),
    };
    let key = match root.open(&subpath) {
        Ok(k) => k,
        Err(e) => return format!("Error listing registry: {e}"),
    };

    let values: Vec<(String, Value)> = match key.values() {
        Ok(iter) => iter.collect(),
        Err(e) => return format!("Error listing registry: {e}"),
    };
    let subkeys: Vec<String> = match key.keys() {
        Ok(iter) => iter.collect(),
        Err(e) => return format!("Error listing registry: {e}"),
    };

    if values.is_empty() && subkeys.is_empty() {
        return format!("Registry key [{path}]:\nNo values or sub-keys found.");
    }

    let mut sections = Vec::new();
    if !values.is_empty() {
        let lines: Vec<String> = values
            .into_iter()
            .map(|(name, value)| format!("  {name} = {}", format_value(value)))
            .collect();
        sections.push(format!("Values:\n{}", lines.join("\n")));
    }
    if !subkeys.is_empty() {
        let lines: Vec<String> = subkeys
            .into_iter()
            .map(|name| format!("  {name}"))
            .collect();
        sections.push(format!("Sub-Keys:\n{}", lines.join("\n")));
    }

    format!("Registry key [{path}]:\n{}", sections.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hkcu_path() {
        let (_, subpath) = parse_registry_path(r"HKCU:\Software\WindowsOperationCliTest").unwrap();
        assert_eq!(subpath, r"Software\WindowsOperationCliTest");
    }

    #[test]
    fn rejects_unknown_root() {
        assert!(parse_registry_path(r"HKXX:\Foo").is_err());
    }

    #[test]
    fn parses_binary_tokens() {
        assert_eq!(parse_bytes("1, 2, 255").unwrap(), vec![1, 2, 255]);
        assert_eq!(parse_bytes("0x0A 0xFF").unwrap(), vec![10, 255]);
        assert!(parse_bytes("256").is_err());
    }
}
