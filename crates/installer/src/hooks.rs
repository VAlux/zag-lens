use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::model::{Component, Conflict, InstallError};

const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
];

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct HookOwnership {
    pub config_path: PathBuf,
    pub created_hooks_object: bool,
    pub created_event_keys: Vec<String>,
    pub hooks: Vec<OwnedHook>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct OwnedHook {
    pub event: String,
    pub command: String,
}

pub(crate) struct HookSetup {
    pub bytes: Vec<u8>,
    pub ownership: HookOwnership,
    pub changed: bool,
}

pub(crate) struct HookUninstall {
    pub bytes: Vec<u8>,
    pub changed: bool,
}

pub(crate) fn setup(
    current: Option<&[u8]>,
    path: &Path,
    binary: &Path,
    component: Component,
    previous: Option<&HookOwnership>,
) -> Result<HookSetup, InstallError> {
    let desired = desired_hooks(binary, component);
    let mut root = parse_json(current, path, component)?;

    if let Some(previous) = previous {
        if previous.config_path != path {
            return Err(InstallError::Conflicts(vec![Conflict {
                component,
                path: path.to_path_buf(),
                message: format!(
                    "the manifest owns hooks in {}, not this path",
                    previous.config_path.display()
                ),
            }]));
        }
        if previous.hooks == desired
            && previous
                .hooks
                .iter()
                .all(|owned| contains_command(&root, &owned.event, &owned.command))
        {
            return Ok(HookSetup {
                bytes: current.unwrap_or_default().to_vec(),
                ownership: previous.clone(),
                changed: false,
            });
        }

        let missing = previous
            .hooks
            .iter()
            .filter(|owned| !contains_command(&root, &owned.event, &owned.command))
            .map(|owned| owned.event.clone())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(InstallError::Conflicts(vec![Conflict {
                component,
                path: path.to_path_buf(),
                message: format!(
                    "owned hook commands were changed or removed for: {}",
                    missing.join(", ")
                ),
            }]));
        }
        remove_owned(&mut root, previous);
    }

    let root_object = root.as_object_mut().expect("parse_json returns an object");
    let created_hooks_object = !root_object.contains_key("hooks");
    let hooks_value = root_object
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_object = hooks_value
        .as_object_mut()
        .ok_or_else(|| InstallError::InvalidConfig {
            component,
            path: path.to_path_buf(),
            message: "the 'hooks' field must be an object".to_owned(),
        })?;

    let mut created_event_keys = Vec::new();
    let mut owned_hooks = Vec::new();
    for desired_hook in desired {
        let created_event = !hooks_object.contains_key(&desired_hook.event);
        let groups = hooks_object
            .entry(desired_hook.event.clone())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| InstallError::InvalidConfig {
                component,
                path: path.to_path_buf(),
                message: format!("hook '{}' must be an array", desired_hook.event),
            })?;

        if contains_command_in_groups(groups, &desired_hook.command) {
            continue;
        }
        groups.push(json!({
            "hooks": [{
                "type": "command",
                "command": desired_hook.command,
            }]
        }));
        if created_event {
            created_event_keys.push(desired_hook.event.clone());
        }
        owned_hooks.push(desired_hook);
    }

    let bytes = encode_json(&root)?;
    let changed = current != Some(bytes.as_slice());
    Ok(HookSetup {
        bytes,
        ownership: HookOwnership {
            config_path: path.to_path_buf(),
            created_hooks_object,
            created_event_keys,
            hooks: owned_hooks,
        },
        changed,
    })
}

pub(crate) fn uninstall(
    current: Option<&[u8]>,
    path: &Path,
    component: Component,
    ownership: &HookOwnership,
) -> Result<HookUninstall, InstallError> {
    if ownership.config_path != path {
        return Err(InstallError::Conflicts(vec![Conflict {
            component,
            path: path.to_path_buf(),
            message: format!(
                "the manifest owns hooks in {}, not this path",
                ownership.config_path.display()
            ),
        }]));
    }
    let Some(current) = current else {
        return Ok(HookUninstall {
            bytes: Vec::new(),
            changed: false,
        });
    };
    let mut root = parse_json(Some(current), path, component)?;
    remove_owned(&mut root, ownership);
    let bytes = encode_json(&root)?;
    Ok(HookUninstall {
        changed: current != bytes,
        bytes,
    })
}

fn desired_hooks(binary: &Path, component: Component) -> Vec<OwnedHook> {
    let (harness, events) = match component {
        Component::Codex => ("codex", CODEX_EVENTS),
        Component::Claude => ("claude", CLAUDE_EVENTS),
        Component::Zellij => unreachable!("Zellij does not use JSON hooks"),
    };
    let executable = shell_quote(&binary.to_string_lossy());
    events
        .iter()
        .map(|event| OwnedHook {
            event: (*event).to_owned(),
            command: format!("{executable} hook --harness {harness} --event {event}"),
        })
        .collect()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn parse_json(
    current: Option<&[u8]>,
    path: &Path,
    component: Component,
) -> Result<Value, InstallError> {
    let Some(bytes) = current.filter(|bytes| !bytes.is_empty()) else {
        return Ok(Value::Object(Map::new()));
    };
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| InstallError::InvalidConfig {
            component,
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if !value.is_object() {
        return Err(InstallError::InvalidConfig {
            component,
            path: path.to_path_buf(),
            message: "the document root must be an object".to_owned(),
        });
    }
    Ok(value)
}

fn encode_json(value: &Value) -> Result<Vec<u8>, InstallError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| InstallError::Serialization(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn contains_command(root: &Value, event: &str, command: &str) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .is_some_and(|groups| contains_command_in_groups(groups, command))
}

fn contains_command_in_groups(groups: &[Value], command: &str) -> bool {
    groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|commands| {
                commands.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook.get("command").and_then(Value::as_str) == Some(command)
                })
            })
    })
}

fn remove_owned(root: &mut Value, ownership: &HookOwnership) {
    let Some(hooks_object) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };

    for owned in &ownership.hooks {
        let Some(groups) = hooks_object
            .get_mut(&owned.event)
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        let mut removed = false;
        for group in groups.iter_mut() {
            let Some(commands) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            if !removed
                && let Some(index) = commands.iter().position(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook.get("command").and_then(Value::as_str)
                            == Some(owned.command.as_str())
                })
            {
                commands.remove(index);
                removed = true;
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|commands| !commands.is_empty())
        });
    }

    for event in &ownership.created_event_keys {
        if hooks_object
            .get(event)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            hooks_object.remove(event);
        }
    }
    if ownership.created_hooks_object && hooks_object.is_empty() {
        root.as_object_mut()
            .expect("JSON root is an object")
            .remove("hooks");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_path_is_shell_quoted() {
        let hooks = desired_hooks(Path::new("/tmp/a'b/zag lens"), Component::Codex);
        assert!(
            hooks[0]
                .command
                .starts_with("'/tmp/a'\"'\"'b/zag lens' hook")
        );
    }

    #[test]
    fn unrelated_hook_is_preserved_during_setup_and_uninstall() {
        let original = br#"{
          "hooks": {
            "Stop": [{"matcher": "x", "hooks": [{"type": "command", "command": "existing"}] }]
          },
          "theme": "dark"
        }"#;
        let setup = setup(
            Some(original),
            Path::new("hooks.json"),
            Path::new("/bin/zag-lens"),
            Component::Codex,
            None,
        )
        .expect("setup succeeds");
        let uninstall = uninstall(
            Some(&setup.bytes),
            Path::new("hooks.json"),
            Component::Codex,
            &setup.ownership,
        )
        .expect("uninstall succeeds");
        let value: Value = serde_json::from_slice(&uninstall.bytes).expect("valid JSON");
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["hooks"]["Stop"][0]["hooks"][0]["command"], "existing");
    }
}
