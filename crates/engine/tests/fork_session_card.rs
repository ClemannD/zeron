//! Fork: the session card plugin's engine calls (src/rpc/fork_session_card.rs)
//! through the real dispatcher.

use std::sync::Arc;

use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::ProjectActionRun;
use zeron_rpc::methods;

/// The card's details come from the chat's checkout, and `npm:<script>`
/// runs that script as a project action: every click a fresh terminal of
/// the chat's, refused for another space's chat or a script that's gone.
#[tokio::test]
async fn the_card_lists_scripts_and_runs_one_as_an_action() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project with spaces");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("package.json"), r#"{"scripts":{"dev":"echo card-test","build":"tsc"}}"#).unwrap();
    let core = EngineCore::assemble(&root.path().join("engine"), Arc::new(HarnessRegistry::new()), zeron_proto::HarnessId::Mock, None).unwrap();
    core.workspace.create_space("space", &core.device_id, project.to_str().unwrap(), None, true).unwrap();
    core.workspace.create_space("other-space", &core.device_id, project.to_str().unwrap(), None, true).unwrap();
    core.workspace.create_chat("chat", Some("space"), None, None, None).unwrap();
    core.workspace.create_chat_hosted("foreign", Some("space"), None, Some("another-device"), None, None, None, None).unwrap();
    let client = zeron_rpc::memory_client(core.rpc_service());

    let card: serde_json::Value = client.call_as("ForkSessionCard", serde_json::json!({ "chatId": "chat" })).await.unwrap();
    let names: Vec<&str> = card["scripts"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["build", "dev"]);
    assert!(client.call("ForkSessionCard", serde_json::json!({ "chatId": "foreign" })).await.is_err());

    let params = serde_json::json!({ "spaceId": "space", "chatId": "chat", "actionId": "npm:dev", "cols": 80, "rows": 24 });
    for invalid in [
        serde_json::json!({ "spaceId": "space", "chatId": "foreign", "actionId": "npm:dev", "cols": 80, "rows": 24 }),
        serde_json::json!({ "spaceId": "other-space", "chatId": "chat", "actionId": "npm:dev", "cols": 80, "rows": 24 }),
        serde_json::json!({ "spaceId": "space", "chatId": "chat", "actionId": "npm:missing", "cols": 80, "rows": 24 }),
    ] {
        assert!(client.call(methods::RUN_PROJECT_ACTION, invalid.clone()).await.is_err(), "must refuse {invalid}");
    }
    let first: ProjectActionRun = client.call_as(methods::RUN_PROJECT_ACTION, params.clone()).await.unwrap();
    let second: ProjectActionRun = client.call_as(methods::RUN_PROJECT_ACTION, params.clone()).await.unwrap();
    assert_ne!(first.terminal.id, second.terminal.id);
    assert_eq!(std::path::Path::new(&first.terminal.cwd), std::fs::canonicalize(&project).unwrap());
    assert_eq!(core.terminals.chats().get(&first.terminal.id).map(String::as_str), Some("chat"));
    for run in [first, second] {
        core.terminals.close(&run.terminal.id).unwrap();
    }
}
