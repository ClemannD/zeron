//! Fork: the engine calls behind the session card plugin
//! (apps/zgx/src/plugins/fork_bridge.rs). Both run on the chat's own device,
//! forwarded there like project actions.
//!
//!   `ForkSessionCard {chatId}` → `{ deviceName?, branch?, base?, additions?,
//!   deletions?, scripts: [{ name, command }], scriptsError? }`: the device's
//!   name, the branch, its line
//!   totals over the repository's default branch (the old desktop card's
//!   `+N −M`), and the checkout's package.json scripts.
//!
//!   `RunProjectAction` with `actionId` `npm:<script>`: that script, run as a
//!   project action is (its own terminal in the chat's checkout), so zgx's
//!   action terminal tabs run it unchanged. The name is resolved again here;
//!   a viewer never sends a shell command.
//!
//! Kept in its own file so rpc.rs carries only the hooks.
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use zeron_proto::{ProjectAction, ProjectActionIcon};
use zeron_rpc::{RpcError, RpcReply, RpcService, methods, parse_params};

use super::{EngineRpc, RunProjectActionParams};

pub(super) const SESSION_CARD: &str = "ForkSessionCard";
const NPM_PREFIX: &str = "npm:";
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024;

/// Ours to forward to the chat's device (`RunProjectAction` already is).
pub(super) fn forwardable(method: &str) -> bool {
    method == SESSION_CARD
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct PackageScript {
    pub name: String,
    pub command: String,
}

impl EngineRpc {
    /// The fork's calls: an answer, or None for the rest of the dispatcher.
    pub(super) async fn fork_session_card(
        &self,
        method: &str,
        params: &Json,
    ) -> Option<Result<RpcReply, RpcError>> {
        match method {
            SESSION_CARD => Some(self.session_card(params).await),
            methods::RUN_PROJECT_ACTION
                if params["actionId"].as_str().is_some_and(|id| id.starts_with(NPM_PREFIX)) =>
            {
                Some(self.run_package_script(params.clone()).await)
            }
            _ => None,
        }
    }

    async fn session_card(&self, params: &Json) -> Result<RpcReply, RpcError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct P {
            chat_id: String,
        }
        let p: P = parse_params(params.clone())?;
        let chat = self
            .workspace
            .chat(&p.chat_id)
            .map_err(|err| RpcError::Failed(err.to_string()))?
            .ok_or_else(|| RpcError::Failed("Chat not found".into()))?;
        if chat.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed("Chat belongs to another device".into()));
        }
        let mut out = json!({ "scripts": [] });
        // This device's name, as the account's device list has it.
        let name = self
            .workspace
            .read_devices()
            .ok()
            .and_then(|devices| devices.into_iter().find(|d| d.id == chat.device_id))
            .map(|d| d.name);
        if let Some(name) = name {
            out["deviceName"] = json!(name);
        }
        if let Some(branch) = &chat.branch {
            out["branch"] = json!(branch);
        }
        let Some(cwd) = chat.cwd.clone() else {
            return RpcReply::value(&out);
        };
        match read_package_scripts(Path::new(&cwd)) {
            Ok(scripts) => out["scripts"] = json!(scripts),
            Err(why) => out["scriptsError"] = json!(why),
        }
        // The branch's totals over its base, as the old card's Branch
        // changes scope had them. Either step failing leaves them out.
        let branches = self
            .call_value(methods::LIST_BRANCHES, json!({ "repoPath": cwd }))
            .await
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok());
        if let Some(base) = branches.and_then(|b| default_base_ref(&b, chat.branch.as_deref())) {
            let diff = self
                .call_value(
                    methods::GET_CHECKOUT_DIFF,
                    json!({ "cwd": cwd, "mode": "branch", "baseRef": base }),
                )
                .await;
            if let Some(diff) = diff {
                out["additions"] = diff["additions"].clone();
                out["deletions"] = diff["deletions"].clone();
            }
            out["base"] = json!(base);
        }
        RpcReply::value(&out)
    }

    /// `RunProjectAction` for `npm:<script>`: the same checks and launch as a
    /// saved action, with the action made from the script found now.
    async fn run_package_script(&self, params: Json) -> Result<RpcReply, RpcError> {
        let p: RunProjectActionParams = parse_params(params)?;
        let name = p.action_id.strip_prefix(NPM_PREFIX).unwrap_or_default();
        let space = self.local_project_action_space(&p.space_id)?;
        let chat = self
            .workspace
            .chat(&p.chat_id)
            .map_err(|err| RpcError::Failed(err.to_string()))?
            .ok_or_else(|| RpcError::Failed("Project chat not found".into()))?;
        if chat.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed("Project chat belongs to another device".into()));
        }
        if chat.space_id.as_deref() != Some(space.id.as_str()) {
            return Err(RpcError::Failed("Project chat belongs to another space".into()));
        }
        let cwd = chat
            .cwd
            .map(PathBuf::from)
            .ok_or_else(|| RpcError::Failed("Project chat has no checkout".into()))?;
        let checkout = self
            .repos
            .workspace_checkout(Path::new(&space.path), &cwd)
            .await
            .ok_or_else(|| RpcError::Failed("Project chat checkout is unavailable".into()))?;
        let project_root = std::fs::canonicalize(&space.path)
            .map_err(|_| RpcError::Failed("Project root is unavailable".into()))?;
        let action = package_script_action(&checkout, name).map_err(RpcError::Failed)?;
        let run = crate::project_actions::launch_project_action(
            &self.terminals,
            &action,
            &project_root,
            &checkout,
            p.cols,
            p.rows,
        )
        .map_err(|err| RpcError::Failed(err.to_string()))?;
        self.terminals
            .associate_chat(&run.terminal.id, &p.chat_id)
            .map_err(|err| RpcError::Failed(err.to_string()))?;
        RpcReply::value(&run)
    }

    /// One of the dispatcher's own unary calls, for its value.
    async fn call_value(&self, method: &str, params: Json) -> Option<Json> {
        match Box::pin(self.handle(method, params)).await {
            Ok(RpcReply::Value(value)) => Some(value),
            _ => None,
        }
    }
}

/// The base a branch is compared with: the repository's first branch unless
/// that's the current one, then main or master (the old desktop's rule).
fn default_base_ref(branches: &[String], current: Option<&str>) -> Option<String> {
    let first = branches.first()?;
    if current != Some(first.as_str()) {
        return Some(first.clone());
    }
    for candidate in ["main", "master"] {
        if branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }
    branches.iter().find(|b| current != Some(b.as_str())).or(Some(first)).cloned()
}

/// A regular file only: never a FIFO or device the repository put there.
fn open_regular(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("expected a regular file"));
    }
    Ok(file)
}

/// The checkout's own package.json scripts (not a parent folder's), by name.
pub(super) fn read_package_scripts(checkout: &Path) -> Result<Vec<PackageScript>, String> {
    let file = match open_regular(&checkout.join("package.json")) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err("package.json exceeds 1 MiB".into());
    }
    let package: Json =
        serde_json::from_slice(&bytes).map_err(|err| format!("Invalid package.json: {err}"))?;
    let Some(scripts) = package.get("scripts").and_then(Json::as_object) else {
        return Ok(Vec::new());
    };
    let mut scripts: Vec<_> = scripts
        .iter()
        .filter_map(|(name, value)| {
            let command = value.as_str()?;
            if name.is_empty() || name.chars().any(char::is_control) || command.trim().is_empty() {
                return None;
            }
            Some(PackageScript { name: name.clone(), command: command.to_owned() })
        })
        .collect();
    scripts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(scripts)
}

/// The action that runs script `name`, resolved again at click time.
pub(super) fn package_script_action(checkout: &Path, name: &str) -> Result<ProjectAction, String> {
    let script = read_package_scripts(checkout)?
        .into_iter()
        .find(|script| script.name == name)
        .ok_or_else(|| "Package script no longer exists".to_string())?;
    #[cfg(not(windows))]
    let argument = format!("'{}'", script.name.replace('\'', "'\"'\"'"));
    #[cfg(windows)]
    let argument = {
        // Both cmd and PowerShell accept this common, literal argument subset.
        if !script.name.chars().all(|c| c.is_ascii_alphanumeric() || "-_:./@".contains(c)) {
            return Err("This script name cannot be run safely in a Windows shell".into());
        }
        format!("\"{}\"", script.name)
    };
    Ok(ProjectAction {
        id: format!("{NPM_PREFIX}{}", script.name),
        name: format!("npm run {}", script.name),
        command: format!("npm run -- {argument}"),
        icon: ProjectActionIcon::Play,
        run_on_worktree_create: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_only_runnable_scripts_from_the_exact_checkout() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("worktree");
        std::fs::create_dir(&checkout).unwrap();
        std::fs::write(root.path().join("package.json"), r#"{"scripts":{"root":"echo root"}}"#).unwrap();
        assert!(read_package_scripts(&checkout).unwrap().is_empty());
        std::fs::write(
            checkout.join("package.json"),
            r#"{"scripts":{"test":"echo test","dev":"vite","bad":42,"empty":"  ","":"no","line\nfeed":"no"}}"#,
        )
        .unwrap();
        let scripts = read_package_scripts(&checkout).unwrap();
        assert_eq!(scripts.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["dev", "test"]);
        assert_eq!(scripts[0].command, "vite");
        let action = package_script_action(&checkout, "dev").unwrap();
        assert_eq!(action.id, "npm:dev");
        assert!(action.command.starts_with("npm run -- "));
        assert!(!action.run_on_worktree_create);
        std::fs::write(checkout.join("package.json"), "{}").unwrap();
        assert!(package_script_action(&checkout, "dev").is_err());
    }

    #[test]
    fn malformed_and_oversized_packages_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        for contents in [b"invalid".to_vec(), vec![b' '; MAX_PACKAGE_BYTES as usize + 1]] {
            std::fs::write(root.path().join("package.json"), contents).unwrap();
            assert!(read_package_scripts(root.path()).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn script_names_are_literal_shell_arguments() {
        let root = tempfile::tempdir().unwrap();
        let name = "dev'$(touch injected); echo oops";
        std::fs::write(
            root.path().join("package.json"),
            serde_json::json!({"scripts": {name: "echo ok"}}).to_string(),
        )
        .unwrap();
        let action = package_script_action(root.path(), name).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("npm() {{ printf '%s\\n' \"$@\"; }}; {}", action.command))
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), format!("run\n--\n{name}\n"));
        assert!(!root.path().join("injected").exists());
    }

    #[test]
    fn the_base_is_the_default_branch_unless_that_is_current() {
        let b = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(default_base_ref(&b(&["main", "feat"]), Some("feat")).as_deref(), Some("main"));
        assert_eq!(default_base_ref(&b(&["feat", "main"]), Some("feat")).as_deref(), Some("main"));
        assert_eq!(default_base_ref(&b(&["main"]), Some("main")).as_deref(), Some("main"));
        assert_eq!(default_base_ref(&[], Some("main")), None);
    }
}
