//! AgenticVisor desktop app. The interface in static/ calls `api` for every
//! request; the answers come from the same functions as the command line tools.

// Keeps a console window from opening next to the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use agenticvisor::api::{self, Ctx};
use agenticvisor::search::index::new_shared;
use serde_json::{json, Value};
use tauri::{Manager, State};

struct AppState {
    ctx: Arc<Ctx>,
}

/// One request from the interface. Returns {"status", "body"}, the same shape the
/// HTTP-style answers had. The work runs off the main thread, so a slow scan does
/// not freeze the window.
#[tauri::command]
async fn api(state: State<'_, AppState>, method: String, target: String, body: Option<Value>) -> Result<Value, String> {
    let ctx = state.ctx.clone();
    let body = body.unwrap_or(Value::Null);
    tauri::async_runtime::spawn_blocking(move || {
        let answer = api::call(&ctx, &method, &target, &body);
        json!({"status": answer.status, "body": answer.body})
    })
    .await
    .map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let ctx = Arc::new(Ctx {
                home: api::home_dir(),
                data_dir: api::default_data_dir(),
                index: new_shared(),
            });
            api::start_initial_index(&ctx);
            app.manage(AppState { ctx });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![api])
        .run(tauri::generate_context!())
        .expect("error while running AgenticVisor");
}
