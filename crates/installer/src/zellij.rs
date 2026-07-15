use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode};
use serde::{Deserialize, Serialize};

use crate::model::{Component, Conflict, InstallError};

const PLUGIN_ALIAS: &str = "zag-lens";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ZellijOwnership {
    pub config_path: PathBuf,
    pub location: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host_binary: String,
    pub added_alias: bool,
    #[serde(default)]
    pub added_host_binary: bool,
    pub added_load_entry: bool,
    pub created_plugins_node: bool,
    pub created_load_plugins_node: bool,
}

#[derive(Debug)]
pub(crate) struct ZellijSetup {
    pub bytes: Vec<u8>,
    pub ownership: ZellijOwnership,
    pub changed: bool,
}

pub(crate) struct ZellijUninstall {
    pub bytes: Vec<u8>,
    pub changed: bool,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn setup(
    current: Option<&[u8]>,
    path: &Path,
    plugin: &Path,
    host_binary: &Path,
    previous: Option<&ZellijOwnership>,
) -> Result<ZellijSetup, InstallError> {
    let desired_location = plugin_location(plugin);
    let desired_host_binary = host_binary.to_string_lossy().into_owned();
    let mut document = parse(current, path)?;
    ensure_single_parent(&document, "plugins", path)?;
    ensure_single_parent(&document, "load_plugins", path)?;

    if let Some(previous) = previous {
        if previous.config_path != path {
            return Err(conflict(
                path,
                format!(
                    "the manifest owns Zellij configuration in {}, not this path",
                    previous.config_path.display()
                ),
            ));
        }
        let alias_matches = find_alias(&document)
            .and_then(alias_location)
            .is_some_and(|location| location == desired_location);
        let load_matches = find_load_entry(&document).is_some();
        let host_binary_matches = find_alias(&document)
            .and_then(alias_host_binary)
            .is_some_and(|binary| binary == desired_host_binary);
        if previous.location == desired_location
            && previous.host_binary == desired_host_binary
            && alias_matches
            && host_binary_matches
            && load_matches
        {
            return Ok(ZellijSetup {
                bytes: current.unwrap_or_default().to_vec(),
                ownership: previous.clone(),
                changed: false,
            });
        }

        if previous.added_alias
            && let Some(alias) = find_alias(&document)
        {
            let current_location = alias_location(alias);
            if current_location != Some(previous.location.as_str()) {
                return Err(conflict(
                    path,
                    "the owned 'zag-lens' plugin alias was edited externally".to_owned(),
                ));
            }
        }
        if previous.added_host_binary
            && let Some(alias) = find_alias(&document)
            && alias_host_binary(alias) != Some(previous.host_binary.as_str())
        {
            return Err(conflict(
                path,
                "the owned 'host_binary' plugin setting was edited externally".to_owned(),
            ));
        }
    }

    let created_plugins_node = document.get("plugins").is_none();
    if created_plugins_node {
        let mut plugins = KdlNode::new("plugins");
        plugins.set_children(KdlDocument::new());
        document.nodes_mut().push(plugins);
    }

    let alias_exists = find_alias(&document).is_some();
    let mut added_alias = false;
    if alias_exists {
        let alias = find_alias_mut(&mut document).expect("alias was found");
        match alias_location(alias) {
            Some(location) if location == desired_location => {}
            Some(location)
                if previous.is_some_and(|ownership| {
                    ownership.added_alias && ownership.location == location
                }) =>
            {
                alias.insert("location", desired_location.clone());
                added_alias = true;
            }
            Some(location) => {
                return Err(conflict(
                    path,
                    format!("plugin alias 'zag-lens' already points to '{location}'"),
                ));
            }
            None => {
                return Err(conflict(
                    path,
                    "plugin alias 'zag-lens' exists without a string location".to_owned(),
                ));
            }
        }
    } else {
        let mut alias = KdlNode::new(PLUGIN_ALIAS);
        alias.insert("location", desired_location.clone());
        document
            .get_mut("plugins")
            .expect("plugins node was ensured")
            .ensure_children()
            .nodes_mut()
            .push(alias);
        added_alias = true;
    }

    let mut added_host_binary = false;
    let alias = find_alias_mut(&mut document).expect("alias was ensured");
    match alias_host_binary(alias) {
        Some(binary) if binary == desired_host_binary => {}
        Some(binary)
            if previous.is_some_and(|ownership| {
                ownership.added_host_binary && ownership.host_binary == binary
            }) =>
        {
            set_alias_host_binary(alias, &desired_host_binary);
            added_host_binary = true;
        }
        Some(binary) => {
            return Err(conflict(
                path,
                format!("plugin alias 'zag-lens' already configures host_binary as '{binary}'"),
            ));
        }
        None => {
            set_alias_host_binary(alias, &desired_host_binary);
            added_host_binary = true;
        }
    }

    let created_load_plugins_node = document.get("load_plugins").is_none();
    if created_load_plugins_node {
        let mut load_plugins = KdlNode::new("load_plugins");
        load_plugins.set_children(KdlDocument::new());
        document.nodes_mut().push(load_plugins);
    }
    let load_exists = find_load_entry(&document).is_some();
    let mut added_load_entry = false;
    if !load_exists {
        document
            .get_mut("load_plugins")
            .expect("load_plugins node was ensured")
            .ensure_children()
            .nodes_mut()
            .push(KdlNode::new(PLUGIN_ALIAS));
        added_load_entry = true;
    }

    document.fmt();
    let bytes = document.to_string().into_bytes();
    let changed = current != Some(bytes.as_slice());
    let ownership = if let Some(previous) = previous {
        ZellijOwnership {
            config_path: path.to_path_buf(),
            location: desired_location,
            host_binary: desired_host_binary,
            added_alias: previous.added_alias || added_alias,
            added_host_binary: previous.added_host_binary || added_host_binary,
            added_load_entry: previous.added_load_entry || added_load_entry,
            created_plugins_node: previous.created_plugins_node || created_plugins_node,
            created_load_plugins_node: previous.created_load_plugins_node
                || created_load_plugins_node,
        }
    } else {
        ZellijOwnership {
            config_path: path.to_path_buf(),
            location: desired_location,
            host_binary: desired_host_binary,
            added_alias,
            added_host_binary,
            added_load_entry,
            created_plugins_node,
            created_load_plugins_node,
        }
    };
    Ok(ZellijSetup {
        bytes,
        ownership,
        changed,
    })
}

pub(crate) fn uninstall(
    current: Option<&[u8]>,
    path: &Path,
    ownership: &ZellijOwnership,
) -> Result<ZellijUninstall, InstallError> {
    if ownership.config_path != path {
        return Err(conflict(
            path,
            format!(
                "the manifest owns Zellij configuration in {}, not this path",
                ownership.config_path.display()
            ),
        ));
    }
    let Some(current) = current else {
        return Ok(ZellijUninstall {
            bytes: Vec::new(),
            changed: false,
        });
    };
    let mut document = parse(Some(current), path)?;

    if ownership.added_alias
        && let Some(alias) = find_alias(&document)
    {
        if alias_location(alias) != Some(ownership.location.as_str()) {
            return Err(conflict(
                path,
                "the owned 'zag-lens' plugin alias was edited externally".to_owned(),
            ));
        }
        if ownership.added_host_binary
            && alias_host_binary(alias) != Some(ownership.host_binary.as_str())
        {
            return Err(conflict(
                path,
                "the owned 'host_binary' plugin setting was edited externally".to_owned(),
            ));
        }
        remove_child(&mut document, "plugins", PLUGIN_ALIAS);
    } else if ownership.added_host_binary
        && let Some(alias) = find_alias(&document)
    {
        if alias_host_binary(alias) != Some(ownership.host_binary.as_str()) {
            return Err(conflict(
                path,
                "the owned 'host_binary' plugin setting was edited externally".to_owned(),
            ));
        }
        remove_alias_host_binary(&mut document);
    }

    if ownership.added_load_entry
        && let Some(load_entry) = find_load_entry(&document)
    {
        if !load_entry.entries().is_empty() || load_entry.children().is_some() {
            return Err(conflict(
                path,
                "the owned 'zag-lens' load entry was edited externally".to_owned(),
            ));
        }
        remove_child(&mut document, "load_plugins", PLUGIN_ALIAS);
    }

    remove_empty_owned_parent(&mut document, "plugins", ownership.created_plugins_node);
    remove_empty_owned_parent(
        &mut document,
        "load_plugins",
        ownership.created_load_plugins_node,
    );
    document.fmt();
    let bytes = document.to_string().into_bytes();
    Ok(ZellijUninstall {
        changed: current != bytes,
        bytes,
    })
}

fn parse(current: Option<&[u8]>, path: &Path) -> Result<KdlDocument, InstallError> {
    let Some(bytes) = current.filter(|bytes| !bytes.is_empty()) else {
        return Ok(KdlDocument::new());
    };
    let text = std::str::from_utf8(bytes).map_err(|error| InstallError::InvalidConfig {
        component: Component::Zellij,
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    text.parse()
        .map_err(|error: kdl::KdlError| InstallError::InvalidConfig {
            component: Component::Zellij,
            path: path.to_path_buf(),
            message: error.to_string(),
        })
}

fn plugin_location(plugin: &Path) -> String {
    format!("file:{}", plugin.to_string_lossy())
}

fn ensure_single_parent(
    document: &KdlDocument,
    name: &str,
    path: &Path,
) -> Result<(), InstallError> {
    if document
        .nodes()
        .iter()
        .filter(|node| node.name().value() == name)
        .count()
        > 1
    {
        return Err(conflict(
            path,
            format!("multiple top-level '{name}' nodes make ownership ambiguous"),
        ));
    }
    Ok(())
}

fn find_alias(document: &KdlDocument) -> Option<&KdlNode> {
    document
        .get("plugins")
        .and_then(KdlNode::children)
        .and_then(|children| children.get(PLUGIN_ALIAS))
}

fn find_alias_mut(document: &mut KdlDocument) -> Option<&mut KdlNode> {
    document
        .get_mut("plugins")
        .and_then(|plugins| plugins.children_mut().as_mut())
        .and_then(|children| children.get_mut(PLUGIN_ALIAS))
}

fn alias_location(alias: &KdlNode) -> Option<&str> {
    alias.get("location")?.value().as_string()
}

fn alias_host_binary(alias: &KdlNode) -> Option<&str> {
    alias
        .children()?
        .get("host_binary")?
        .get(0)?
        .value()
        .as_string()
}

fn set_alias_host_binary(alias: &mut KdlNode, host_binary: &str) {
    let children = alias.ensure_children();
    if let Some(setting) = children.get_mut("host_binary") {
        setting.clear_entries();
        setting.push(host_binary);
    } else {
        let mut setting = KdlNode::new("host_binary");
        setting.push(host_binary);
        children.nodes_mut().push(setting);
    }
}

fn remove_alias_host_binary(document: &mut KdlDocument) {
    let Some(children) = find_alias_mut(document).and_then(|alias| alias.children_mut().as_mut())
    else {
        return;
    };
    children
        .nodes_mut()
        .retain(|node| node.name().value() != "host_binary");
}

fn find_load_entry(document: &KdlDocument) -> Option<&KdlNode> {
    document
        .get("load_plugins")
        .and_then(KdlNode::children)
        .and_then(|children| children.get(PLUGIN_ALIAS))
}

fn remove_child(document: &mut KdlDocument, parent: &str, child: &str) {
    let Some(children) = document
        .get_mut(parent)
        .and_then(|node| node.children_mut().as_mut())
    else {
        return;
    };
    children
        .nodes_mut()
        .retain(|node| node.name().value() != child);
}

fn remove_empty_owned_parent(document: &mut KdlDocument, name: &str, owned: bool) {
    if !owned {
        return;
    }
    let empty = document
        .get(name)
        .and_then(KdlNode::children)
        .is_none_or(|children| children.nodes().is_empty());
    if empty {
        document
            .nodes_mut()
            .retain(|node| node.name().value() != name);
    }
}

fn conflict(path: &Path, message: String) -> InstallError {
    InstallError::Conflicts(vec![Conflict {
        component: Component::Zellij,
        path: path.to_path_buf(),
        message,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_and_uninstall_preserve_unrelated_nodes() {
        let original =
            b"plugins {\n    status-bar location=\"zellij:status-bar\"\n}\ntheme \"catppuccin\"\n";
        let setup = setup(
            Some(original),
            Path::new("config.kdl"),
            Path::new("/tmp/zag-lens.wasm"),
            Path::new("/tmp/zag-lens"),
            None,
        )
        .expect("setup succeeds");
        let output = std::str::from_utf8(&setup.bytes).expect("UTF-8");
        assert!(output.contains("status-bar"));
        assert!(output.contains("zag-lens location=\"file:/tmp/zag-lens.wasm\""));
        assert!(output.contains("host_binary \"/tmp/zag-lens\""));
        assert!(output.contains("theme \"catppuccin\""));

        let uninstall = uninstall(
            Some(&setup.bytes),
            Path::new("config.kdl"),
            &setup.ownership,
        )
        .expect("uninstall succeeds");
        let output = std::str::from_utf8(&uninstall.bytes).expect("UTF-8");
        assert!(output.contains("status-bar"));
        assert!(!output.contains("zag-lens"));
        assert!(output.contains("theme \"catppuccin\""));
    }

    #[test]
    fn setup_reports_existing_alias_conflict() {
        let current = b"plugins {\n    zag-lens location=\"file:/other.wasm\"\n}\n";
        let error = setup(
            Some(current),
            Path::new("config.kdl"),
            Path::new("/tmp/zag-lens.wasm"),
            Path::new("/tmp/zag-lens"),
            None,
        )
        .expect_err("conflict expected");
        assert!(matches!(error, InstallError::Conflicts(_)));
    }
}
