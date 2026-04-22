//! Rust MCP server for pmetra — standalone, no Node.js required.
//!
//! Two roles in one binary:
//!
//!   ROLE 1 — WebSocket broker (port 9001, background task)
//!     Hosts ws://localhost:9001. The WASM app connects out to it on startup.
//!     Keeps one live connection; routes replies by sequence number.
//!
//!   ROLE 2 — MCP server (stdio, foreground)
//!     Speaks JSON-RPC over stdio. Claude Desktop / claude CLI connect here.
//!     Exposes: list_resources, get_resource, set_resource, screenshot.
//!
//! Usage:
//!   cargo run -p pmetra-mcp-server
//!   just mcp-rs
//!
//! Register in claude_desktop_config.json:
//!   { "mcpServers": { "pmetra": { "command": "/path/to/pmetra-mcp-server" } } }
//!
//! Environment:
//!   WS_PORT   — WebSocket port (default: 9001)

use std::{
    collections::HashMap,
    sync::{Arc, atomic::{AtomicU64, Ordering}},
};

use futures_util::{SinkExt, StreamExt};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tracing::{info, warn};

const DEFAULT_WS_PORT: u16 = 9001;

// ─── Shared broker state ───────────────────────────────────────────────────────

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

#[derive(Clone)]
struct Broker {
    /// Sink to send messages to the WASM client. None when WASM is not connected.
    tx: Arc<Mutex<Option<futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >>>>,
    /// Pending reply map: seq → oneshot sender.
    pending: PendingMap,
    /// Monotonic sequence counter.
    seq: Arc<AtomicU64>,
}

impl Broker {
    fn new() -> Self {
        Self {
            tx: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Send a command to the WASM bridge and wait for the reply (5s timeout).
    async fn call(&self, mut cmd: Value) -> Result<Value, String> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        cmd["_seq"] = Value::Number(seq.into());

        let (tx, rx) = oneshot::channel::<Value>();
        self.pending.lock().await.insert(seq, tx);

        // Send
        {
            let mut guard = self.tx.lock().await;
            let sink = guard.as_mut().ok_or("WASM app not connected")?;
            let text = serde_json::to_string(&cmd).map_err(|e| e.to_string())?;
            sink.send(Message::Text(text.into()))
                .await
                .map_err(|e| {
                    e.to_string()
                })?;
        }

        // Wait for reply (5s timeout)
        tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .map_err(|_| {
                // Clean up pending entry on timeout
                let pending = self.pending.clone();
                tokio::spawn(async move { pending.lock().await.remove(&seq); });
                "reply timeout".to_string()
            })?
            .map_err(|_| "reply channel dropped".to_string())
    }
}

// ─── WebSocket broker task ─────────────────────────────────────────────────────

async fn run_ws_broker(broker: Broker, port: u16) {
    // Bind on all interfaces so phones on the same WiFi can reach ws://<mac-ip>:<port>.
    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await
        .unwrap_or_else(|e| panic!("WS broker: cannot bind {addr}: {e}"));
    info!("WebSocket broker listening on ws://{addr} (reachable over LAN)");

    // Accept connections in a loop — WASM reconnects if page reloads.
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        info!("WASM app connected from {peer}");

        let ws = match accept_async(stream).await {
            Ok(ws) => ws,
            Err(e) => {
                warn!("WS handshake failed from {peer}: {e:?}");
                continue;
            }
        };

        let (sink, mut source) = ws.split();
        // Store sink so MCP tools can send through it.
        *broker.tx.lock().await = Some(sink);

        let pending = broker.pending.clone();
        let tx_ref = broker.tx.clone();

        // Read loop — routes replies to waiting oneshot channels.
        tokio::spawn(async move {
            while let Some(msg) = source.next().await {
                let Ok(Message::Text(text)) = msg else { continue };
                let Ok(val) = serde_json::from_str::<Value>(&text) else {
                    warn!("WS: bad JSON from WASM"); continue;
                };
                if let Some(seq) = val.get("_seq").and_then(|v| v.as_u64()) {
                    if let Some(sender) = pending.lock().await.remove(&seq) {
                        let _ = sender.send(val);
                    }
                }
            }
            // WASM disconnected — clear the sink.
            info!("WASM app disconnected");
            *tx_ref.lock().await = None;
        });
    }
}

// ─── Tool parameter types ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GetParams {
    /// Short name of the resource or component.
    /// Examples: "TowerExtension", "GlobalAmbientLight", "Material:TowerExtension"
    /// Use list_resources first to discover all available names.
    name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SetParams {
    /// Short name of the resource or component to patch.
    name: String,
    /// JSON object with fields to update. Omitted fields keep their current values.
    /// Examples:
    ///   {"tower_length": 3}
    ///   {"brightness": 1200}
    ///   {"LinearRgba": {"red":0.0,"green":0.0,"blue":0.1,"alpha":1.0}}
    ///   {"base_color":{"Srgba":{"red":1.0,"green":0.84,"blue":0.0,"alpha":1.0}},"metallic":1.0}
    ///   {"translation":[1.0,0.0,0.0],"scale":[2.0,2.0,2.0]}
    ///   {"context":{"paused":true}}
    value: Value,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct LoadShapeParams {
    /// Display name for the loaded shape, e.g. "cube".
    name: String,
    /// Full Truck JSON content (CompressedShell format).
    data: String,
    /// Optional position: {"translation": [x, y, z]}.
    transform: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct NameOnlyParams {
    /// Name of the shape (as used in load_shape/load_step).
    name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct LoadStepParams {
    /// Display name for the loaded model, e.g. "bracket".
    name: String,
    /// Full STEP file content (ISO-10303-21 format).
    data: String,
    /// Optional position: {"translation": [x, y, z]}.
    transform: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SimulateTouchParams {
    /// X coord of touchstart in canvas-local pixels (from canvas top-left).
    x: f64,
    /// Y coord of touchstart in canvas-local pixels.
    y: f64,
    /// X coord of touchend. Omit for a tap (same as start).
    end_x: Option<f64>,
    /// Y coord of touchend. Omit for a tap.
    end_y: Option<f64>,
    /// Total drag duration in milliseconds. Default 300.
    duration_ms: Option<f64>,
}

// ─── MCP server ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct PmetraServer {
    broker: Broker,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl PmetraServer {
    fn new(broker: Broker) -> Self {
        Self { broker, tool_router: Self::tool_router() }
    }

    /// List all resources, components, and materials currently exposed by the
    /// running pmetra WASM app. Returns a JSON array of short names.
    /// CAD param components only appear when that model variant is active.
    /// Material keys have the form "Material:<ComponentName>".
    #[tool(name = "list_resources")]
    async fn list_resources(&self) -> String {
        match self.broker.call(serde_json::json!({ "cmd": "list" })).await {
            Ok(resp) => resp.get("value")
                .map(|v| v.to_string())
                .unwrap_or_else(|| resp.to_string()),
            Err(e) => format!(r#"{{"error":"{e}"}}"#),
        }
    }

    /// Get the current JSON value of a named resource, component, or material.
    /// Returns a JSON object keyed by the full Rust type path.
    #[tool(name = "get_resource")]
    async fn get_resource(&self, params: Parameters<GetParams>) -> String {
        match self.broker.call(serde_json::json!({
            "cmd": "get",
            "resource": params.0.name
        })).await {
            Ok(resp) => resp.get("value")
                .map(|v| v.to_string())
                .unwrap_or_else(|| resp.to_string()),
            Err(e) => format!(r#"{{"error":"{e}"}}"#),
        }
    }

    /// Patch a resource, component, or material. Only provided fields are changed.
    /// Triggers geometry rebuild for CAD param types (TowerExtension, ExpNurbs, etc.).
    #[tool(name = "set_resource")]
    async fn set_resource(&self, params: Parameters<SetParams>) -> String {
        match self.broker.call(serde_json::json!({
            "cmd": "set",
            "resource": params.0.name,
            "value": params.0.value
        })).await {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }

    /// Capture a PNG screenshot of the current Bevy 3D viewport.
    /// Returns a base64 data URL: "data:image/png;base64,...".
    /// Use this to visually verify geometry after making changes.
    #[tool(name = "screenshot")]
    async fn screenshot(&self) -> String {
        match self.broker.call(serde_json::json!({ "cmd": "screenshot" })).await {
            Ok(resp) => resp.get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => format!("error: {e}"),
        }
    }

    /// Load a Truck JSON shape file into the scene as a rendered mesh.
    /// The shape can be edited and saved back via save_shape.
    #[tool(name = "load_shape")]
    async fn load_shape(&self, params: Parameters<LoadShapeParams>) -> String {
        let mut cmd = serde_json::json!({
            "cmd": "load_shape",
            "name": params.0.name,
            "data": params.0.data,
        });
        if let Some(t) = &params.0.transform { cmd["transform"] = t.clone(); }
        match self.broker.call(cmd).await {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }

    /// Save a loaded TruckModel entity back to Truck JSON format.
    #[tool(name = "save_shape")]
    async fn save_shape(&self, params: Parameters<NameOnlyParams>) -> String {
        match self.broker.call(serde_json::json!({
            "cmd": "save_shape",
            "name": params.0.name,
        })).await {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }

    /// List all loaded Truck shapes (both JSON and STEP).
    /// Returns name and format for each loaded model.
    #[tool(name = "list_shapes")]
    async fn list_shapes(&self) -> String {
        match self.broker.call(serde_json::json!({ "cmd": "list_shapes" })).await {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }

    /// Load a STEP file into the scene as rendered mesh(es).
    /// STEP models are view-only — raw STEP data is stored for re-export.
    #[tool(name = "load_step")]
    async fn load_step(&self, params: Parameters<LoadStepParams>) -> String {
        let mut cmd = serde_json::json!({
            "cmd": "load_step",
            "name": params.0.name,
            "data": params.0.data,
        });
        if let Some(t) = &params.0.transform { cmd["transform"] = t.clone(); }
        match self.broker.call(cmd).await {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }

    /// Save a loaded StepModel entity's raw STEP data.
    #[tool(name = "save_step")]
    async fn save_step(&self, params: Parameters<NameOnlyParams>) -> String {
        match self.broker.call(serde_json::json!({
            "cmd": "save_step",
            "name": params.0.name,
        })).await {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }

    /// Delete a loaded shape — despawns entity from the scene and removes
    /// from browser localStorage. Use list_shapes to see what's loaded.
    #[tool(name = "delete_shape")]
    async fn delete_shape(&self, params: Parameters<NameOnlyParams>) -> String {
        match self.broker.call(serde_json::json!({
            "cmd": "delete_shape",
            "name": params.0.name,
        })).await {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }

    /// Dispatch a synthetic touch on the canvas. Coords are pixels from the
    /// canvas top-left. Omit end_x/end_y for a tap; provide them for a drag.
    /// Blocks until the dispatch finishes (duration_ms + 50ms) so a following
    /// screenshot sees the post-drag state.
    #[tool(name = "simulate_touch")]
    async fn simulate_touch(&self, params: Parameters<SimulateTouchParams>) -> String {
        let dur = params.0.duration_ms.unwrap_or(300.0);
        let end_x = params.0.end_x.unwrap_or(params.0.x);
        let end_y = params.0.end_y.unwrap_or(params.0.y);
        let result = self.broker.call(serde_json::json!({
            "cmd": "simulate_touch",
            "x": params.0.x,
            "y": params.0.y,
            "end_x": end_x,
            "end_y": end_y,
            "duration_ms": dur,
        })).await;
        // Wait for the JS-side dispatch loop to finish so screenshots see
        // the final state rather than a mid-drag frame.
        let wait_ms = (dur as u64).saturating_add(50);
        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
        match result {
            Ok(resp) => resp.to_string(),
            Err(e) => format!(r#"{{"ok":false,"error":"{e}"}}"#),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PmetraServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::default()
    }
}

// ─── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("pmetra_mcp_server=info".parse()?)
        )
        .with_writer(std::io::stderr)
        .init();

    let ws_port: u16 = std::env::var("WS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WS_PORT);

    let broker = Broker::new();

    // WebSocket broker runs for the lifetime of the process regardless of
    // whether an MCP client is connected.
    let ws_handle = tokio::spawn(run_ws_broker(broker.clone(), ws_port));

    // MCP over stdio — serve() blocks until stdin closes (MCP client disconnects).
    // Errors here (e.g. no piped client) are non-fatal; the WS broker keeps running.
    info!("pmetra MCP server (Rust) ready — waiting for MCP client on stdio");
    let server = PmetraServer::new(broker);
    match server.serve(stdio()).await {
        Ok(service) => {
            tokio::select! {
                _ = service.waiting() => info!("MCP client disconnected"),
                _ = ws_handle => {}
            }
        }
        Err(e) => {
            // No MCP client connected (e.g. run directly in terminal).
            // Keep alive so WASM bridge can still connect.
            info!("MCP stdio not connected ({e}) — running as WS-only broker");
            ws_handle.await.ok();
        }
    }
    Ok(())
}
