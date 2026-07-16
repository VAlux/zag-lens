use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Component, Conflict, InstallError};

const BINARY_MARKER: &str = "__ZAG_LENS_BINARY_JSON__";
const PLUGIN_TEMPLATE: &str = r#"const zagLensBinary = __ZAG_LENS_BINARY_JSON__

function stringValue(value) {
  return typeof value === "string" && value.length > 0 ? value : undefined
}

function send(eventType, payload) {
  if (!process.env.ZELLIJ_PANE_ID) return
  try {
    const child = Bun.spawn(
      [zagLensBinary, "hook", "--harness", "opencode", "--event", eventType],
      {
        env: process.env,
        stdin: "pipe",
        stdout: "ignore",
        stderr: "ignore",
      },
    )
    child.stdin.write(JSON.stringify({ event_type: eventType, ...payload }))
    child.stdin.end()
    void child.exited.catch(() => {})
  } catch {
    // Zag Lens is observational and MUST NOT interrupt OpenCode.
  }
}

export const ZagLensPlugin = async () => ({
  event: async ({ event }) => {
    try {
      const properties = event?.properties ?? {}
      switch (event?.type) {
        case "session.created":
        case "session.deleted": {
          const sessionID = stringValue(properties.info?.id)
          if (sessionID) send(event.type, { session_id: sessionID })
          return
        }
        case "session.status": {
          const sessionID = stringValue(properties.sessionID)
          const status = stringValue(properties.status?.type)
          if (sessionID && (status === "busy" || status === "retry")) {
            send(event.type, { session_id: sessionID, status })
          }
          return
        }
        case "permission.asked":
        case "question.asked":
        case "question.replied":
        case "question.rejected": {
          const sessionID = stringValue(properties.sessionID)
          if (sessionID) send(event.type, { session_id: sessionID })
          return
        }
        case "permission.replied": {
          const sessionID = stringValue(properties.sessionID)
          const reply = stringValue(properties.reply)
          if (sessionID && reply) send(event.type, { session_id: sessionID, reply })
          return
        }
        case "message.updated": {
          const info = properties.info ?? {}
          const sessionID = stringValue(info.sessionID)
          const turnID = stringValue(info.id)
          if (
            info.role === "assistant" &&
            sessionID &&
            turnID &&
            typeof info.time?.completed === "number"
          ) {
            send(event.type, {
              session_id: sessionID,
              turn_id: turnID,
              completed: true,
              has_error: info.error !== undefined,
            })
          }
          return
        }
        case "session.error": {
          const sessionID = stringValue(properties.sessionID)
          if (!sessionID) return
          const errorName = stringValue(properties.error?.name)
          send(event.type, {
            session_id: sessionID,
            ...(errorName ? { error_name: errorName } : {}),
          })
          return
        }
        default:
          return
      }
    } catch {
      // Ignore malformed or future native events without affecting OpenCode.
    }
  },
})
"#;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct OpenCodeOwnership {
    pub plugin_path: PathBuf,
    pub contents: String,
    pub owned_file: bool,
}

pub(crate) struct OpenCodeSetup {
    pub bytes: Vec<u8>,
    pub ownership: OpenCodeOwnership,
    pub changed: bool,
}

pub(crate) struct OpenCodeUninstall {
    pub changed: bool,
}

pub(crate) fn setup(
    current: Option<&[u8]>,
    path: &Path,
    binary: &Path,
    previous: Option<&OpenCodeOwnership>,
) -> Result<OpenCodeSetup, InstallError> {
    let desired = render_plugin(binary)?;
    if let Some(previous) = previous {
        validate_path(path, previous)?;
        let previous_bytes = previous.contents.as_bytes();
        if current != Some(previous_bytes) {
            return Err(conflict(
                path,
                "the installed OpenCode plugin was changed outside Zag Lens",
            ));
        }
        if !previous.owned_file && desired.as_slice() != previous_bytes {
            return Err(conflict(
                path,
                "the matching pre-existing OpenCode plugin is not owned by Zag Lens",
            ));
        }
        return Ok(OpenCodeSetup {
            changed: current != Some(desired.as_slice()),
            ownership: OpenCodeOwnership {
                plugin_path: path.to_path_buf(),
                contents: String::from_utf8(desired.clone())
                    .expect("generated OpenCode plugin is UTF-8"),
                owned_file: previous.owned_file,
            },
            bytes: desired,
        });
    }

    match current {
        None => Ok(OpenCodeSetup {
            bytes: desired.clone(),
            ownership: OpenCodeOwnership {
                plugin_path: path.to_path_buf(),
                contents: String::from_utf8(desired).expect("generated plugin is UTF-8"),
                owned_file: true,
            },
            changed: true,
        }),
        Some(current) if current == desired => Ok(OpenCodeSetup {
            bytes: desired.clone(),
            ownership: OpenCodeOwnership {
                plugin_path: path.to_path_buf(),
                contents: String::from_utf8(desired).expect("generated plugin is UTF-8"),
                owned_file: false,
            },
            changed: false,
        }),
        Some(_) => Err(conflict(
            path,
            "an unmanaged OpenCode plugin already exists at the Zag Lens path",
        )),
    }
}

pub(crate) fn uninstall(
    current: Option<&[u8]>,
    path: &Path,
    ownership: &OpenCodeOwnership,
) -> Result<OpenCodeUninstall, InstallError> {
    validate_path(path, ownership)?;
    if !ownership.owned_file || current.is_none() {
        return Ok(OpenCodeUninstall { changed: false });
    }
    if current != Some(ownership.contents.as_bytes()) {
        return Err(conflict(
            path,
            "the installed OpenCode plugin was changed outside Zag Lens",
        ));
    }
    Ok(OpenCodeUninstall { changed: true })
}

fn render_plugin(binary: &Path) -> Result<Vec<u8>, InstallError> {
    let binary = binary.to_string_lossy();
    let encoded = serde_json::to_string(binary.as_ref())
        .map_err(|error| InstallError::Serialization(error.to_string()))?;
    Ok(PLUGIN_TEMPLATE
        .replace(BINARY_MARKER, &encoded)
        .into_bytes())
}

fn validate_path(path: &Path, ownership: &OpenCodeOwnership) -> Result<(), InstallError> {
    if ownership.plugin_path == path {
        return Ok(());
    }
    Err(conflict(
        path,
        &format!(
            "the manifest owns an OpenCode plugin in {}, not this path",
            ownership.plugin_path.display()
        ),
    ))
}

fn conflict(path: &Path, message: &str) -> InstallError {
    InstallError::Conflicts(vec![Conflict {
        component: Component::OpenCode,
        path: path.to_path_buf(),
        message: message.to_owned(),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_plugin_uses_direct_spawn_and_allowlisted_fields() {
        let plugin = String::from_utf8(
            render_plugin(Path::new("/tmp/a'b/zag lens")).expect("render plugin"),
        )
        .expect("UTF-8 plugin");
        assert!(plugin.contains(r#"const zagLensBinary = "/tmp/a'b/zag lens""#));
        assert!(plugin.contains("Bun.spawn("));
        assert!(plugin.contains("stdin: \"pipe\""));
        assert!(!plugin.contains("Bun.$"));
        assert!(!plugin.contains("JSON.stringify(event)"));
        for forbidden in [
            "patterns",
            "metadata",
            "questions",
            "assistant_message",
            "tool_input",
            "tool_result",
            "responseBody",
        ] {
            assert!(!plugin.contains(forbidden), "{forbidden}");
        }
    }
}
