//! Generic JS ↔ Bevy bridge + WebSocket client.
//!
//! Two independent modes — either works standalone:
//!
//! MODE 1 — Playwright / browser console:
//!   window.pmetra.set("CadGeneratedModelSpawner", '{"selected_params":"ExpNurbsSolid"}')
//!   window.pmetra.get("CadGeneratedModelSpawner")  // → JSON string
//!   window.pmetra.list()                            // → JSON array of resource names
//!
//! MODE 2 — MCP WebSocket (connects to ws://localhost:9001/ws if available):
//!   Server sends: {"cmd":"set","resource":"CadGeneratedModelSpawner","value":{"selected_params":"ExpNurbsSolid"}}
//!   Server sends: {"cmd":"get","resource":"CadGeneratedModelSpawner"}
//!   Server sends: {"cmd":"list"}
//!   WASM replies:  {"ok":true,"value":{...}}  or  {"ok":false,"error":"..."}
//!
//! Both paths feed the same internal Mutex queue → same Bevy system applies changes.
//! Zero hardcoded resource names, field names, or enum variants anywhere.
//! sync_resource_cache auto-discovers all resources registered with ReflectResource.

use std::sync::Mutex;

use bevy::{
    prelude::*,
    reflect::{
        serde::{ReflectDeserializer, ReflectSerializer, TypedReflectDeserializer},
        ReflectMut, TypeRegistry,
    },
};
use serde::de::DeserializeSeed;
use serde_json::Value;
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, WebSocket};

use crate::truck_loader::{self, StepModel, TruckModel};

// ---------------------------------------------------------------------------
// Spawn registry — lets plugin.rs register type-specific spawn functions
// without making the bridge depend on pmetra types directly.
// ---------------------------------------------------------------------------

/// A function that spawns a new CAD model entity given a JSON value patch and a Transform.
/// The fn receives the world, a patch Value (fields to override on the type default), and a
/// Transform. It must spawn the entity AND generate the geometry (fire events as needed).
pub type BridgeSpawnFn = Box<dyn Fn(&mut World, &Value, Transform) + Send + Sync>;

/// Resource registered by the app to let the bridge spawn specific CAD model types.
/// Insert one entry per component name that you want the bridge to be able to spawn.
#[derive(Resource, Default)]
pub struct BridgeSpawnRegistry {
    pub spawners: std::collections::HashMap<String, BridgeSpawnFn>,
}

// ---------------------------------------------------------------------------
// Internal command queue  (JS/WS → Bevy frame)
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum BridgeCommand {
    Set { resource: String, value: Value, seq: Option<u64> },
    Get { resource: String, seq: Option<u64> },
    List { seq: Option<u64> },
    Screenshot { seq: Option<u64> },
    /// Despawn all entities that have `component` (optionally only the nth via `index`).
    /// {"cmd":"despawn","component":"TowerExtension"}          → despawn all
    /// {"cmd":"despawn","component":"TowerExtension","index":1} → despawn entity[1] only
    Despawn { component: String, index: Option<usize>, seq: Option<u64> },
    /// Spawn a new entity with `component` set to `value` (JSON patch over type default).
    /// Requires a `BridgeSpawnRegistry` resource to be present (registered in plugin.rs).
    /// {"cmd":"spawn","component":"TowerExtension","value":{"height":2.0},"transform":{"translation":[1,0,0]}}
    Spawn { component: String, value: Value, transform_json: Option<Value>, remove_existing: bool, seq: Option<u64> },
    /// Load a Truck JSON string, tessellate it, and spawn a mesh entity.
    /// {"cmd":"load_shape","name":"cube","data":"{...json...}","transform":{"translation":[0,0,0]}}
    LoadShape { name: String, data: String, transform_json: Option<Value>, seq: Option<u64> },
    /// Save a TruckModel entity back to Truck JSON.
    /// {"cmd":"save_shape","name":"cube"}
    SaveShape { name: String, seq: Option<u64> },
    /// List all loaded TruckModel entity names.
    /// {"cmd":"list_shapes"}
    ListShapes { seq: Option<u64> },
    /// Load a STEP file string, tessellate, and spawn mesh entities.
    /// {"cmd":"load_step","name":"cube","data":"ISO-10303-21;..."}
    LoadStep { name: String, data: String, transform_json: Option<Value>, seq: Option<u64> },
    /// Save a StepModel entity's raw STEP data back.
    /// {"cmd":"save_step","name":"cube"}
    SaveStep { name: String, seq: Option<u64> },
    /// Delete a loaded shape — despawns entity and removes from localStorage.
    /// {"cmd":"delete_shape","name":"cube"}
    DeleteShape { name: String, seq: Option<u64> },
    /// Dispatch a synthetic touch-drag on the canvas at the given coords (pixels
    /// from canvas top-left). If `end_x`/`end_y` equal start, behaves as a tap.
    /// {"cmd":"simulate_touch","x":100,"y":200,"end_x":150,"end_y":200,"duration_ms":300}
    SimulateTouch { x: f64, y: f64, end_x: f64, end_y: f64, duration_ms: f64, seq: Option<u64> },
    /// Return field-level type info for a registered type (or all types if name is "*").
    /// {"cmd":"schema","name":"TowerExtension"}   → single type schema
    /// {"cmd":"schema"}  or  {"cmd":"schema","name":"*"}  → all registered schemas
    Schema { name: String, seq: Option<u64> },
}

static COMMAND_QUEUE: Mutex<Vec<BridgeCommand>> = Mutex::new(Vec::new());
/// Serialized resource values — updated by Bevy each PostUpdate, read by JS / WS replies.
static RESOURCE_CACHE: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Loaded shape names — updated by apply_bridge_commands, read by JS list_shapes().
static SHAPE_CACHE: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new()); // (name, format)
/// Type schemas — populated once from Bevy's TypeRegistry, read by JS schema() and WS.
static SCHEMA_CACHE: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn push_cmd(cmd: BridgeCommand) {
    match COMMAND_QUEUE.lock() {
        Ok(mut q) => q.push(cmd),
        Err(e) => warn!("wasm_bridge: COMMAND_QUEUE lock poisoned: {e}"),
    }
}

fn cache_get(resource: &str) -> String {
    RESOURCE_CACHE
        .lock()
        .ok()
        .and_then(|c| c.iter().find(|(k, _)| k == resource).map(|(_, v)| v.clone()))
        .unwrap_or_default()
}

fn capture_screenshot() -> String {
    let Ok(window) = js_sys::global().dyn_into::<web_sys::Window>() else {
        return String::new();
    };
    let Some(document) = window.document() else { return String::new() };
    let canvas = document
        .query_selector("canvas")
        .ok()
        .flatten()
        .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok());
    canvas
        .and_then(|c| c.to_data_url_with_type("image/png").ok())
        .unwrap_or_default()
}

fn cache_list() -> String {
    RESOURCE_CACHE
        .lock()
        .map(|c| {
            let names: Vec<&str> = c.iter().map(|(k, _)| k.as_str()).collect();
            serde_json::to_string(&names).unwrap_or_default()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// localStorage persistence — shapes survive page reloads
// ---------------------------------------------------------------------------

const LS_PREFIX: &str = "pmetra_shape:";
const LS_STEP_PREFIX: &str = "pmetra_step:";

fn get_local_storage() -> Option<web_sys::Storage> {
    js_sys::global()
        .dyn_into::<web_sys::Window>()
        .ok()?
        .local_storage()
        .ok()?
}

/// Persist a shape's JSON data to localStorage.
fn persist_shape(name: &str, json: &str, is_step: bool) {
    let prefix = if is_step { LS_STEP_PREFIX } else { LS_PREFIX };
    if let Some(storage) = get_local_storage() {
        if let Err(e) = storage.set_item(&format!("{prefix}{name}"), json) {
            warn!("wasm_bridge: localStorage set failed: {e:?}");
        }
    }
}

/// Remove a shape from localStorage.
fn remove_persisted_shape(name: &str, is_step: bool) {
    let prefix = if is_step { LS_STEP_PREFIX } else { LS_PREFIX };
    if let Some(storage) = get_local_storage() {
        storage.remove_item(&format!("{prefix}{name}")).ok();
    }
}

/// Read all persisted shapes from localStorage and queue LoadShape/LoadStep commands.
fn restore_persisted_shapes() {
    let Some(storage) = get_local_storage() else { return };
    let len: u32 = match storage.length() {
        Ok(n) => n,
        Err(_) => return,
    };
    let mut count = 0u32;
    for i in 0..len {
        let key: String = match storage.key(i) {
            Ok(Some(k)) => k,
            _ => continue,
        };
        let data: String = match storage.get_item(&key) {
            Ok(Some(d)) => d,
            _ => continue,
        };
        if let Some(name) = key.strip_prefix(LS_PREFIX) {
            push_cmd(BridgeCommand::LoadShape {
                name: name.to_string(),
                data,
                transform_json: None,
                seq: None,
            });
            count += 1;
        } else if let Some(name) = key.strip_prefix(LS_STEP_PREFIX) {
            push_cmd(BridgeCommand::LoadStep {
                name: name.to_string(),
                data,
                transform_json: None,
                seq: None,
            });
            count += 1;
        }
    }
    if count > 0 {
        info!("wasm_bridge: restoring {count} persisted shapes from localStorage");
    }
}

// ---------------------------------------------------------------------------
// MODE 1 — JS API (window.pmetra.*)
// ---------------------------------------------------------------------------

pub fn mount_js_namespace() {
    let Ok(window) = js_sys::global().dyn_into::<web_sys::Window>() else {
        warn!("wasm_bridge: no window — skipping JS namespace");
        return;
    };
    let obj = js_sys::Object::new();

    // pmetra.set("ResourceName", '{"field":"value"}')
    let set_fn = Closure::wrap(Box::new(|resource: JsValue, json: JsValue| {
        let (Some(r), Some(j)) = (resource.as_string(), json.as_string()) else { return };
        match serde_json::from_str::<Value>(&j) {
            Ok(v) => push_cmd(BridgeCommand::Set { resource: r, value: v, seq: None }),
            Err(e) => web_sys::console::warn_1(&format!("pmetra.set: bad JSON: {e}").into()),
        }
    }) as Box<dyn FnMut(JsValue, JsValue)>);
    js_sys::Reflect::set(&obj, &"set".into(), set_fn.as_ref()).ok();
    set_fn.forget();

    // pmetra.get("ResourceName") → JSON string
    let get_fn = Closure::wrap(Box::new(|resource: JsValue| -> JsValue {
        resource.as_string().map(|r| JsValue::from_str(&cache_get(&r))).unwrap_or(JsValue::NULL)
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    js_sys::Reflect::set(&obj, &"get".into(), get_fn.as_ref()).ok();
    get_fn.forget();

    // pmetra.list() → JSON array string
    let list_fn =
        Closure::wrap(Box::new(|| JsValue::from_str(&cache_list())) as Box<dyn FnMut() -> JsValue>);
    js_sys::Reflect::set(&obj, &"list".into(), list_fn.as_ref()).ok();
    list_fn.forget();

    // pmetra.screenshot() → PNG data URL string ("data:image/png;base64,...")
    // Captures the Bevy render canvas — AI can use this to see the current scene.
    let screenshot_fn = Closure::wrap(Box::new(|| -> JsValue {
        let Ok(window) = js_sys::global().dyn_into::<web_sys::Window>() else {
            return JsValue::NULL;
        };
        let document = window.document().unwrap();
        // Bevy renders into a <canvas> element — find the first one on the page.
        let canvas = document
            .query_selector("canvas")
            .ok()
            .flatten()
            .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok());
        match canvas {
            Some(c) => c
                .to_data_url_with_type("image/png")
                .map(|s| JsValue::from_str(s.as_str()))
                .unwrap_or(JsValue::NULL),
            None => JsValue::NULL,
        }
    }) as Box<dyn FnMut() -> JsValue>);
    js_sys::Reflect::set(&obj, &"screenshot".into(), screenshot_fn.as_ref()).ok();
    screenshot_fn.forget();

    // pmetra.despawn("ComponentName")          → despawn all entities with that component
    // pmetra.despawn("ComponentName", 1)       → despawn entity[1] only
    let despawn_fn = Closure::wrap(Box::new(|component: JsValue, index: JsValue| {
        let Some(c) = component.as_string() else { return };
        let idx = index.as_f64().map(|n| n as usize);
        push_cmd(BridgeCommand::Despawn { component: c, index: idx, seq: None });
    }) as Box<dyn FnMut(JsValue, JsValue)>);
    js_sys::Reflect::set(&obj, &"despawn".into(), despawn_fn.as_ref()).ok();
    despawn_fn.forget();

    // pmetra.spawn("ComponentName", '{"field":value}')
    // pmetra.spawn("ComponentName", '{}', '{"translation":[1,0,0]}', false)
    let spawn_fn = Closure::wrap(Box::new(
        |component: JsValue, json: JsValue, transform_json: JsValue, remove_existing: JsValue| {
            let Some(c) = component.as_string() else { return };
            let value = json.as_string()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(Value::Object(Default::default()));
            let transform_json = transform_json.as_string()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let remove_existing = remove_existing.as_bool().unwrap_or(false);
            push_cmd(BridgeCommand::Spawn { component: c, value, transform_json, remove_existing, seq: None });
        },
    ) as Box<dyn FnMut(JsValue, JsValue, JsValue, JsValue)>);
    js_sys::Reflect::set(&obj, &"spawn".into(), spawn_fn.as_ref()).ok();
    spawn_fn.forget();

    // pmetra.schema("TowerExtension") → JSON schema for one type (sync from cache)
    // pmetra.schema()  or  pmetra.schema("*") → all schemas
    let schema_fn = Closure::wrap(Box::new(|name: JsValue| -> JsValue {
        let n = name.as_string().unwrap_or_else(|| "*".to_string());
        JsValue::from_str(&schema_get(&n))
    }) as Box<dyn FnMut(JsValue) -> JsValue>);
    js_sys::Reflect::set(&obj, &"schema".into(), schema_fn.as_ref()).ok();
    schema_fn.forget();

    // pmetra.load_shape("name", '{"boundaries":...}')         → queues load
    // pmetra.load_shape("name", '{"boundaries":...}', '{"translation":[1,0,0]}')
    let load_shape_fn = Closure::wrap(Box::new(
        |name: JsValue, data: JsValue, transform_json: JsValue| {
            let (Some(n), Some(d)) = (name.as_string(), data.as_string()) else { return };
            let tj = transform_json
                .as_string()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            push_cmd(BridgeCommand::LoadShape {
                name: n,
                data: d,
                transform_json: tj,
                seq: None,
            });
        },
    ) as Box<dyn FnMut(JsValue, JsValue, JsValue)>);
    js_sys::Reflect::set(&obj, &"load_shape".into(), load_shape_fn.as_ref()).ok();
    load_shape_fn.forget();

    // pmetra.save_shape("name") → queues save (result returned via WS)
    let save_shape_fn = Closure::wrap(Box::new(|name: JsValue| {
        let Some(n) = name.as_string() else { return };
        push_cmd(BridgeCommand::SaveShape { name: n, seq: None });
    }) as Box<dyn FnMut(JsValue)>);
    js_sys::Reflect::set(&obj, &"save_shape".into(), save_shape_fn.as_ref()).ok();
    save_shape_fn.forget();

    // pmetra.delete_shape("name") → despawn entity + remove from localStorage
    let delete_shape_fn = Closure::wrap(Box::new(|name: JsValue| {
        let Some(n) = name.as_string() else { return };
        push_cmd(BridgeCommand::DeleteShape { name: n, seq: None });
    }) as Box<dyn FnMut(JsValue)>);
    js_sys::Reflect::set(&obj, &"delete_shape".into(), delete_shape_fn.as_ref()).ok();
    delete_shape_fn.forget();

    // pmetra.list_shapes() → returns JSON array of {name, format} from cache (sync)
    let list_shapes_fn = Closure::wrap(Box::new(|| -> JsValue {
        let json = SHAPE_CACHE
            .lock()
            .map(|c| {
                let items: Vec<serde_json::Value> = c.iter()
                    .map(|(n, f)| serde_json::json!({"name": n, "format": f}))
                    .collect();
                serde_json::to_string(&items).unwrap_or_default()
            })
            .unwrap_or_default();
        JsValue::from_str(&json)
    }) as Box<dyn FnMut() -> JsValue>);
    js_sys::Reflect::set(&obj, &"list_shapes".into(), list_shapes_fn.as_ref()).ok();
    list_shapes_fn.forget();

    // pmetra.load_step("name", "ISO-10303-21;...")
    let load_step_fn = Closure::wrap(Box::new(
        |name: JsValue, data: JsValue, transform_json: JsValue| {
            let (Some(n), Some(d)) = (name.as_string(), data.as_string()) else { return };
            let tj = transform_json
                .as_string()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            push_cmd(BridgeCommand::LoadStep {
                name: n,
                data: d,
                transform_json: tj,
                seq: None,
            });
        },
    ) as Box<dyn FnMut(JsValue, JsValue, JsValue)>);
    js_sys::Reflect::set(&obj, &"load_step".into(), load_step_fn.as_ref()).ok();
    load_step_fn.forget();

    // pmetra.save_step("name")
    let save_step_fn = Closure::wrap(Box::new(|name: JsValue| {
        let Some(n) = name.as_string() else { return };
        push_cmd(BridgeCommand::SaveStep { name: n, seq: None });
    }) as Box<dyn FnMut(JsValue)>);
    js_sys::Reflect::set(&obj, &"save_step".into(), save_step_fn.as_ref()).ok();
    save_step_fn.forget();

    js_sys::Reflect::set(window.as_ref(), &"pmetra".into(), &obj).ok();
    info!("wasm_bridge: window.pmetra mounted");
}

// ---------------------------------------------------------------------------
// MODE 2 — WebSocket client (optional, connects to MCP server)
// ---------------------------------------------------------------------------

// Keep the WebSocket alive for the lifetime of the app.
static WS_HANDLE: Mutex<Option<WebSocket>> = Mutex::new(None);
/// Target URL for WS reconnect.
static WS_URL: Mutex<Option<String>> = Mutex::new(None);

/// Try to open a WebSocket connection. If it fails or drops, the reconnect
/// timer (`start_ws_reconnect_timer`) will retry automatically.
fn try_ws_connect(url: &str) {
    // Don't connect if we already have a live connection.
    if let Ok(handle) = WS_HANDLE.lock() {
        if let Some(ws) = handle.as_ref() {
            if ws.ready_state() == WebSocket::OPEN || ws.ready_state() == WebSocket::CONNECTING {
                return;
            }
        }
    }

    let ws = match WebSocket::new(url) {
        Ok(ws) => ws,
        Err(_) => return, // silently skip — reconnect timer will retry
    };

    // onmessage → parse JSON command → push to queue
    let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
        let Some(text) = e.data().as_string() else { return };
        match serde_json::from_str::<Value>(&text) {
            Ok(msg) => handle_ws_message(msg),
            Err(e) => warn!("wasm_bridge: WS bad JSON: {e}"),
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    let onerror = Closure::wrap(Box::new(move |_| {
        // Clear handle so reconnect timer can retry.
        if let Ok(mut h) = WS_HANDLE.lock() { *h = None; }
    }) as Box<dyn FnMut(JsValue)>);
    ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    onerror.forget();

    let onclose = Closure::wrap(Box::new(move |_: JsValue| {
        info!("wasm_bridge: WS disconnected — will auto-reconnect");
        if let Ok(mut h) = WS_HANDLE.lock() { *h = None; }
    }) as Box<dyn FnMut(JsValue)>);
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();

    let onopen = Closure::wrap(Box::new(move || {
        info!("wasm_bridge: WS connected to MCP server");
    }) as Box<dyn FnMut()>);
    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();

    if let Ok(mut handle) = WS_HANDLE.lock() {
        *handle = Some(ws);
    }
}

/// Initial connection + periodic reconnect timer (every 3s).
pub fn connect_websocket(url: &str) {
    // Store URL for reconnect timer.
    if let Ok(mut u) = WS_URL.lock() { *u = Some(url.to_string()); }

    // Try first connection immediately.
    try_ws_connect(url);

    // If the first connection failed, log once (not on every retry).
    if let Ok(handle) = WS_HANDLE.lock() {
        if handle.is_none() {
            info!("wasm_bridge: WS not connected (MCP server not running) — will auto-reconnect");
        }
    }

    // Reconnect timer — checks every 3s, only connects if handle is None.
    let reconnect = Closure::wrap(Box::new(move || {
        let url = WS_URL.lock().ok().and_then(|u| u.clone());
        if let Some(url) = url {
            try_ws_connect(&url);
        }
    }) as Box<dyn FnMut()>);
    let Ok(window) = js_sys::global().dyn_into::<web_sys::Window>() else { return };
    window.set_interval_with_callback_and_timeout_and_arguments_0(
        reconnect.as_ref().unchecked_ref(), 3_000,
    ).ok();
    reconnect.forget();
}

fn handle_ws_message(msg: Value) {
    let Some(cmd) = msg.get("cmd").and_then(|v| v.as_str()) else {
        warn!("wasm_bridge: WS message missing 'cmd'");
        return;
    };
    let seq = msg.get("_seq").and_then(|v| v.as_u64());
    match cmd {
        "set" => {
            let resource = msg.get("resource").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let value = msg.get("value").cloned().unwrap_or(Value::Null);
            push_cmd(BridgeCommand::Set { resource, value, seq });
        }
        "get" => {
            let resource = msg.get("resource").and_then(|v| v.as_str()).unwrap_or("").to_string();
            push_cmd(BridgeCommand::Get { resource, seq });
        }
        "list" => push_cmd(BridgeCommand::List { seq }),
        "screenshot" => push_cmd(BridgeCommand::Screenshot { seq }),
        "despawn" => {
            let component = msg.get("component").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let index = msg.get("index").and_then(|v| v.as_u64()).map(|n| n as usize);
            push_cmd(BridgeCommand::Despawn { component, index, seq });
        }
        "spawn" => {
            let component = msg.get("component").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let value = msg.get("value").cloned().unwrap_or(Value::Object(Default::default()));
            let transform_json = msg.get("transform").cloned();
            let remove_existing = msg.get("remove_existing").and_then(|v| v.as_bool()).unwrap_or(false);
            push_cmd(BridgeCommand::Spawn { component, value, transform_json, remove_existing, seq });
        }
        "load_shape" => {
            let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let data = msg.get("data").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let transform_json = msg.get("transform").cloned();
            push_cmd(BridgeCommand::LoadShape { name, data, transform_json, seq });
        }
        "save_shape" => {
            let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            push_cmd(BridgeCommand::SaveShape { name, seq });
        }
        "list_shapes" => push_cmd(BridgeCommand::ListShapes { seq }),
        "load_step" => {
            let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let data = msg.get("data").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let transform_json = msg.get("transform").cloned();
            push_cmd(BridgeCommand::LoadStep { name, data, transform_json, seq });
        }
        "save_step" => {
            let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            push_cmd(BridgeCommand::SaveStep { name, seq });
        }
        "delete_shape" => {
            let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            push_cmd(BridgeCommand::DeleteShape { name, seq });
        }
        "simulate_touch" => {
            let x = msg.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = msg.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let end_x = msg.get("end_x").and_then(|v| v.as_f64()).unwrap_or(x);
            let end_y = msg.get("end_y").and_then(|v| v.as_f64()).unwrap_or(y);
            let duration_ms = msg.get("duration_ms").and_then(|v| v.as_f64()).unwrap_or(300.0);
            push_cmd(BridgeCommand::SimulateTouch { x, y, end_x, end_y, duration_ms, seq });
        }
        "schema" => {
            let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("*").to_string();
            push_cmd(BridgeCommand::Schema { name, seq });
        }
        other => warn!("wasm_bridge: unknown WS cmd '{other}'"),
    }
}

fn ws_send(msg: &str) {
    match WS_HANDLE.lock() {
        Ok(handle) => {
            if let Some(ws) = handle.as_ref() {
                if let Err(e) = ws.send_with_str(msg) {
                    warn!("wasm_bridge: ws_send failed: {e:?}");
                }
            }
        }
        Err(e) => warn!("wasm_bridge: WS_HANDLE lock poisoned: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Bevy systems
// ---------------------------------------------------------------------------

/// Drain queue, apply to ECS, send WS replies.
/// Runs in PreUpdate so change-detection is visible to all Update systems.
fn apply_bridge_commands(world: &mut World) {
    let cmds: Vec<BridgeCommand> = {
        let Ok(mut q) = COMMAND_QUEUE.lock() else { return };
        std::mem::take(&mut *q)
    };
    if cmds.is_empty() {
        return;
    }

    let registry = world.get_resource::<AppTypeRegistry>().map(|r| r.clone());
    let Some(registry) = registry else { return };

    for cmd in cmds {
        match cmd {
            BridgeCommand::Set { resource, value, seq } => {
                let ok = apply_json_to_resource(world, &registry.read(), &resource, &value);
                let mut reply = serde_json::json!({
                    "ok": ok,
                    "cmd": "set",
                    "resource": resource,
                });
                if !ok { reply["error"] = "failed".into(); }
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::Get { resource, seq } => {
                let json_str = cache_get(&resource);
                let value: Value = serde_json::from_str(&json_str).unwrap_or(Value::Null);
                let mut reply = serde_json::json!({
                    "ok": true,
                    "cmd": "get",
                    "resource": resource,
                    "value": value,
                });
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::List { seq } => {
                let json_str = cache_list();
                let value: Value = serde_json::from_str(&json_str).unwrap_or(Value::Array(vec![]));
                let mut reply = serde_json::json!({
                    "ok": true,
                    "cmd": "list",
                    "value": value,
                });
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::Screenshot { seq } => {
                let data_url = capture_screenshot();
                let mut reply = serde_json::json!({
                    "ok": true,
                    "cmd": "screenshot",
                    "value": data_url,
                });
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::Despawn { component, index, seq } => {
                let ok = despawn_entities(world, &registry.read(), &component, index);
                let mut reply = serde_json::json!({
                    "ok": ok,
                    "cmd": "despawn",
                    "component": component,
                });
                if !ok { reply["error"] = "not found".into(); }
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::Spawn { component, value, transform_json, remove_existing, seq } => {
                let ok = spawn_via_registry(world, &registry.read(), &component, &value, transform_json.as_ref(), remove_existing);
                let mut reply = serde_json::json!({
                    "ok": ok,
                    "cmd": "spawn",
                    "component": component,
                });
                if !ok { reply["error"] = "no spawner registered for this component".into(); }
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::LoadShape { name, data, transform_json, seq } => {
                let transform = parse_transform_json(transform_json.as_ref());
                info!("wasm_bridge: load_shape '{}' ({} bytes)", name, data.len());
                let result = truck_loader::spawn_from_json(world, &name, &data, transform);
                let mut reply = match &result {
                    Ok(entity) => {
                        if let Ok(mut c) = SHAPE_CACHE.lock() {
                            c.retain(|(n, _)| n != &name);
                            c.push((name.clone(), "json".to_string()));
                        }
                        persist_shape(&name, &data, false);
                        info!("wasm_bridge: load_shape '{}' → {:?}", name, entity);
                        serde_json::json!({
                            "ok": true, "cmd": "load_shape", "name": name,
                            "entity": format!("{entity:?}"),
                        })
                    }
                    Err(e) => {
                        warn!("wasm_bridge: load_shape '{}' FAILED: {:#}", name, e);
                        serde_json::json!({
                            "ok": false, "cmd": "load_shape", "error": format!("{e:#}"),
                        })
                    }
                };
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::SaveShape { name, seq } => {
                let entity_opt = {
                    let mut q = world.query::<(Entity, &TruckModel)>();
                    q.iter(world).find(|(_, m)| m.name == name).map(|(e, _)| e)
                };
                let mut reply = match entity_opt {
                    Some(entity) => match truck_loader::save_entity_json(world, entity) {
                        Ok(json) => serde_json::json!({
                            "ok": true, "cmd": "save_shape", "name": name, "data": json,
                        }),
                        Err(e) => serde_json::json!({
                            "ok": false, "cmd": "save_shape", "error": format!("{e:#}"),
                        }),
                    },
                    None => serde_json::json!({
                        "ok": false, "cmd": "save_shape", "error": format!("no TruckModel named '{name}'"),
                    }),
                };
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::ListShapes { seq } => {
                let mut q_json = world.query::<&TruckModel>();
                let mut q_step = world.query::<&StepModel>();
                let json_names: Vec<Value> = q_json.iter(world)
                    .map(|m| serde_json::json!({"name": m.name, "format": "json"}))
                    .collect();
                let step_names: Vec<Value> = q_step.iter(world)
                    .map(|m| serde_json::json!({"name": m.name, "format": "step"}))
                    .collect();
                let mut all = json_names;
                all.extend(step_names);
                // Sync the SHAPE_CACHE from actual ECS state
                if let Ok(mut c) = SHAPE_CACHE.lock() {
                    *c = all.iter()
                        .filter_map(|v| {
                            let name = v.get("name")?.as_str()?.to_string();
                            let format = v.get("format")?.as_str()?.to_string();
                            Some((name, format))
                        })
                        .collect();
                }
                let mut reply = serde_json::json!({
                    "ok": true, "cmd": "list_shapes", "value": all,
                });
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::LoadStep { name, data, transform_json, seq } => {
                let transform = parse_transform_json(transform_json.as_ref());
                info!("wasm_bridge: load_step '{}' ({} bytes)", name, data.len());
                let result = truck_loader::spawn_from_step(world, &name, &data, transform);
                let mut reply = match &result {
                    Ok(entities) => {
                        if let Ok(mut c) = SHAPE_CACHE.lock() {
                            c.retain(|(n, _)| n != &name);
                            c.push((name.clone(), "step".to_string()));
                        }
                        persist_shape(&name, &data, true);
                        info!("wasm_bridge: load_step '{}' → {} meshes", name, entities.len());
                        serde_json::json!({
                            "ok": true, "cmd": "load_step", "name": name,
                            "count": entities.len(),
                        })
                    }
                    Err(e) => {
                        warn!("wasm_bridge: load_step '{}' FAILED: {:#}", name, e);
                        serde_json::json!({
                            "ok": false, "cmd": "load_step", "error": format!("{e:#}"),
                        })
                    }
                };
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::SaveStep { name, seq } => {
                let entity_opt = {
                    let mut q = world.query::<(Entity, &StepModel)>();
                    q.iter(world).find(|(_, m)| m.name == name).map(|(e, _)| e)
                };
                let mut reply = match entity_opt {
                    Some(entity) => match truck_loader::save_entity_step(world, entity) {
                        Ok(data) => serde_json::json!({
                            "ok": true, "cmd": "save_step", "name": name, "data": data,
                        }),
                        Err(e) => serde_json::json!({
                            "ok": false, "cmd": "save_step", "error": format!("{e:#}"),
                        }),
                    },
                    None => serde_json::json!({
                        "ok": false, "cmd": "save_step",
                        "error": format!("no StepModel named '{name}'"),
                    }),
                };
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::DeleteShape { name, seq } => {
                // Despawn TruckModel entities with this name
                let json_entities: Vec<Entity> = {
                    let mut q = world.query::<(Entity, &TruckModel)>();
                    q.iter(world).filter(|(_, m)| m.name == name).map(|(e, _)| e).collect()
                };
                // Despawn StepModel entities with this name
                let step_entities: Vec<Entity> = {
                    let mut q = world.query::<(Entity, &StepModel)>();
                    q.iter(world).filter(|(_, m)| m.name == name).map(|(e, _)| e).collect()
                };
                let count = json_entities.len() + step_entities.len();
                for e in json_entities.into_iter().chain(step_entities) {
                    world.despawn(e);
                }
                // Remove from caches and localStorage
                if let Ok(mut c) = SHAPE_CACHE.lock() {
                    c.retain(|(n, _)| n != &name);
                }
                remove_persisted_shape(&name, false);
                remove_persisted_shape(&name, true);
                info!("wasm_bridge: delete_shape '{}' — despawned {count} entities", name);
                let mut reply = serde_json::json!({
                    "ok": count > 0,
                    "cmd": "delete_shape",
                    "name": name,
                    "count": count,
                });
                if count == 0 { reply["error"] = format!("no shape named '{name}'").into(); }
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::SimulateTouch { x, y, end_x, end_y, duration_ms, seq } => {
                let dispatched = dispatch_simulated_touch(x, y, end_x, end_y, duration_ms);
                let mut reply = serde_json::json!({
                    "ok": dispatched,
                    "cmd": "simulate_touch",
                    "x": x, "y": y, "end_x": end_x, "end_y": end_y,
                    "duration_ms": duration_ms,
                });
                if !dispatched { reply["error"] = "TouchEvent not supported or canvas missing".into(); }
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
            BridgeCommand::Schema { name, seq } => {
                // Ensure cache is populated (no-op after first call).
                populate_schema_cache(&registry.read());
                let json_str = schema_get(&name);
                let value: Value = serde_json::from_str(&json_str).unwrap_or(Value::Null);
                let mut reply = serde_json::json!({
                    "ok": !value.is_null(),
                    "cmd": "schema",
                    "name": name,
                    "value": value,
                });
                if value.is_null() { reply["error"] = format!("type '{name}' not found").into(); }
                if let Some(s) = seq { reply["_seq"] = s.into(); }
                ws_send(&reply.to_string());
            }
        }
    }
}

/// Invoke the `window._pmetraSimulateTouch` JS helper defined in index.html.
/// Fires touchstart → touchmoves → touchend on the canvas.
/// Returns false if the helper is missing or the browser lacks TouchEvent support.
fn dispatch_simulated_touch(x: f64, y: f64, end_x: f64, end_y: f64, duration_ms: f64) -> bool {
    let Ok(window) = js_sys::global().dyn_into::<web_sys::Window>() else { return false };
    let Ok(helper) = js_sys::Reflect::get(&window, &"_pmetraSimulateTouch".into()) else {
        return false;
    };
    let Ok(func) = helper.dyn_into::<js_sys::Function>() else { return false };
    let args = js_sys::Array::of5(
        &JsValue::from_f64(x),
        &JsValue::from_f64(y),
        &JsValue::from_f64(end_x),
        &JsValue::from_f64(end_y),
        &JsValue::from_f64(duration_ms),
    );
    match func.apply(&JsValue::NULL, &args) {
        Ok(ret) => ret.as_bool().unwrap_or(true),
        Err(e) => {
            warn!("wasm_bridge: simulate_touch call failed: {e:?}");
            false
        }
    }
}

/// Parse a `{"translation":[x,y,z]}` JSON value into a Transform.
fn parse_transform_json(v: Option<&Value>) -> Transform {
    v.and_then(|v| {
        let arr = v.get("translation")?.as_array()?;
        if arr.len() >= 3 {
            let x = arr[0].as_f64()? as f32;
            let y = arr[1].as_f64()? as f32;
            let z = arr[2].as_f64()? as f32;
            Some(Transform::from_translation(Vec3::new(x, y, z)))
        } else {
            None
        }
    })
    .unwrap_or_default()
}

/// Despawn entities that carry `component_name`.
/// `index = None`    → despawn all matching entities.
/// `index = Some(n)` → despawn only the nth entity (0-based).
fn despawn_entities(
    world: &mut World,
    registry: &TypeRegistry,
    component_name: &str,
    index: Option<usize>,
) -> bool {
    let Some(type_reg) = registry
        .iter()
        .find(|r| r.type_info().type_path_table().short_path() == component_name)
    else {
        warn!("wasm_bridge despawn: type '{component_name}' not in registry");
        return false;
    };
    let Some(reflect_component) = type_reg.data::<ReflectComponent>().cloned() else {
        warn!("wasm_bridge despawn: '{component_name}' has no ReflectComponent");
        return false;
    };
    let all_entities: Vec<Entity> = world.query::<Entity>().iter(world).collect();
    let matches: Vec<Entity> = all_entities
        .into_iter()
        .filter(|&id| {
            world
                .get_entity(id)
                .ok()
                .map(|eref| reflect_component.contains(eref))
                .unwrap_or(false)
        })
        .collect();

    if matches.is_empty() {
        warn!("wasm_bridge despawn: no entity has component '{component_name}'");
        return false;
    }

    let to_despawn: Vec<Entity> = match index {
        None => matches,
        Some(n) => match matches.get(n) {
            Some(&id) => vec![id],
            None => {
                warn!("wasm_bridge despawn: no entity[{n}] for '{component_name}'");
                return false;
            }
        },
    };

    for id in to_despawn {
        world.entity_mut(id).despawn();
    }
    info!("wasm_bridge: despawned {component_name} (index={index:?})");
    true
}

/// Spawn a new CAD model entity via the `BridgeSpawnRegistry`.
/// If no spawner is registered for `component_name`, returns false.
fn spawn_via_registry(
    world: &mut World,
    registry: &TypeRegistry,
    component_name: &str,
    patch: &Value,
    transform_json: Option<&Value>,
    remove_existing: bool,
) -> bool {
    let transform = parse_transform_json(transform_json);

    if remove_existing {
        despawn_entities(world, registry, component_name, None);
    }

    // Temporarily remove the spawn registry so we can mutate the world inside the spawner.
    // The spawner fn is borrowed from the local; world is free to mutate.
    let Some(reg) = world.remove_resource::<BridgeSpawnRegistry>() else {
        warn!("wasm_bridge spawn: BridgeSpawnRegistry not present — register it in plugin.rs");
        return false;
    };

    let Some(spawner) = reg.spawners.get(component_name) else {
        warn!("wasm_bridge spawn: no spawner registered for '{component_name}'");
        world.insert_resource(reg);
        return false;
    };

    spawner(world, patch, transform); // world is free; reg is owned locally
    world.insert_resource(reg);       // put registry back
    info!("wasm_bridge: spawned {component_name}");
    true
}

/// Create a `BridgeSpawnFn` for any reflected `Component + Default` type `T`.
///
/// The returned closure deserializes `T::default()` + JSON patch via Bevy reflection,
/// then calls `on_spawn(world, params, transform)` with the concrete T value.
/// This lets `plugin.rs` fire the typed `GenerateCadModel<T>` event without making
/// the bridge depend on pmetra types.
///
/// # Example (in plugin.rs)
/// ```rust
/// registry.spawners.insert("TowerExtension".into(),
///     make_spawner_with::<TowerExtension, _>(|world, params, transform| {
///         world.write_message(GenerateCadModel { params, transform, remove_existing_models: false });
///     })
/// );
/// ```
pub(crate) fn make_spawner_with<T, F>(on_spawn: F) -> BridgeSpawnFn
where
    T: Component + Default + Reflect + TypePath + 'static,
    F: Fn(&mut World, T, Transform) + Send + Sync + 'static,
{
    Box::new(move |world: &mut World, patch: &Value, transform: Transform| {
        let mut params = T::default();

        // Only do the reflection round-trip if there are fields to patch.
        let has_patch = patch.as_object().map_or(false, |m| !m.is_empty());
        if has_patch {
            let registry = world.resource::<AppTypeRegistry>().clone();
            let reg = registry.read();
            if let Ok(mut cur_val) =
                serde_json::to_value(ReflectSerializer::new(&params as &dyn Reflect, &reg))
            {
                merge_into_inner(&mut cur_val, patch);
                if let Ok(s) = serde_json::to_string(&cur_val) {
                    let mut de = serde_json::Deserializer::from_str(&s);
                    if let Ok(reflected) = ReflectDeserializer::new(&reg).deserialize(&mut de) {
                        params.apply(&*reflected);
                    }
                }
            }
        }

        on_spawn(world, params, transform);
    })
}

/// Parse an optional `[n]` index suffix from a name like `"TowerExtension[1]"`.
/// Returns `(base_name, index)` where index defaults to 0 if absent.
fn parse_index_suffix(name: &str) -> (&str, usize) {
    if let Some(bracket) = name.rfind('[') {
        if name.ends_with(']') {
            let idx_str = &name[bracket + 1..name.len() - 1];
            if let Ok(n) = idx_str.parse::<usize>() {
                return (&name[..bracket], n);
            }
        }
    }
    (name, 0)
}

/// Apply a JSON patch to a reflected Bevy resource, component, or material.
///
/// Dispatch:
///   "Material:<ComponentName>[n]"  → nth entity via that component → patch StandardMaterial asset
///   anything else                  → Resource first, then Component fallback
///
/// Index suffix `[n]` selects the nth entity (0-based) when multiple entities share a
/// component type (e.g. MultiModels2TowerExtensions has two TowerExtension entities).
/// Omitting the suffix is equivalent to `[0]`.
///
/// Strategy: serialize current → deep-merge patch → deserialize back → apply.
/// Works for structs, enums, newtypes, and arbitrary nesting with zero special-casing.
/// Patch fields are relative to the type's inner fields (no type-path wrapper needed).
fn apply_json_to_resource(
    world: &mut World,
    registry: &TypeRegistry,
    short_name: &str,
    patch: &Value,
) -> bool {
    // ── Material path — "Material:ComponentName[n]" ────────────────────────
    if let Some(rest) = short_name.strip_prefix("Material:") {
        let (comp_name, entity_index) = parse_index_suffix(rest);
        let Some(type_reg) = registry
            .iter()
            .find(|r| r.type_info().type_path_table().short_path() == comp_name)
        else {
            warn!("wasm_bridge: component '{comp_name}' not in registry");
            return false;
        };
        let Some(reflect_component) = type_reg.data::<ReflectComponent>().cloned() else {
            warn!("wasm_bridge: '{comp_name}' has no ReflectComponent");
            return false;
        };
        // Find nth entity with that pmetra component
        let all_entities: Vec<Entity> = world.query::<Entity>().iter(world).collect();
        let matching: Vec<Entity> = all_entities.iter().copied().filter(|&id| {
            world.get_entity(id).ok()
                .map(|eref| reflect_component.contains(eref))
                .unwrap_or(false)
        }).collect();
        let Some(&entity_id) = matching.get(entity_index) else {
            warn!("wasm_bridge: no entity[{entity_index}] has component '{comp_name}'");
            return false;
        };
        // Clone the handle — check entity itself then direct children
        let mat_handle = {
            let candidates: Vec<Entity> = {
                let mut v = vec![entity_id];
                if let Ok(eref) = world.get_entity(entity_id) {
                    if let Some(children) = eref.get::<Children>() {
                        for child in children.iter() { v.push(child); }
                    }
                }
                v
            };
            let found = candidates.iter().copied().find_map(|eid| {
                world
                    .get_entity(eid)
                    .ok()?
                    .get::<MeshMaterial3d<StandardMaterial>>()
                    .map(|h| h.id())
            });
            let Some(handle) = found else {
                warn!("wasm_bridge: entity with '{comp_name}' has no StandardMaterial (checked self + children)");
                return false;
            };
            handle
        };
        // StandardMaterial lacks #[reflect(Deserialize)] so full ReflectDeserializer
        // can't reconstruct it. Patch field-by-field via TypedReflectDeserializer instead.
        {
            let mut assets = world.resource_mut::<Assets<StandardMaterial>>();
            let Some(material) = assets.get_mut(mat_handle) else {
                warn!("wasm_bridge: material asset not found");
                return false;
            };
            apply_patch_to_struct(material as &mut dyn Reflect, patch, registry);
        }
        info!("wasm_bridge: patched Material:{comp_name}");
        return true;
    }

    // ── Parse optional [n] index suffix ───────────────────────────────────
    let (base_name, entity_index) = parse_index_suffix(short_name);

    // ── Look up type in registry ───────────────────────────────────────────
    let Some(type_reg) = registry
        .iter()
        .find(|r| r.type_info().type_path_table().short_path() == base_name)
    else {
        warn!("wasm_bridge: type '{base_name}' not in registry");
        return false;
    };

    // ── Resource path (index ignored — resources are singletons) ──────────
    if let Some(reflect_resource) = type_reg.data::<ReflectResource>() {
        let merged_str = {
            let Ok(cur) = reflect_resource.reflect(&*world) else {
                warn!("wasm_bridge: '{short_name}' not in world");
                return false;
            };
            let Ok(mut cur_val) = serde_json::to_value(ReflectSerializer::new(cur, registry)) else {
                warn!("wasm_bridge: serialize failed for '{short_name}'");
                return false;
            };
            merge_into_inner(&mut cur_val, patch);
            let Ok(s) = serde_json::to_string(&cur_val) else { return false };
            s
        };
        let mut de = serde_json::Deserializer::from_str(&merged_str);
        let Ok(new_val) = ReflectDeserializer::new(registry).deserialize(&mut de) else {
            warn!("wasm_bridge: deserialize failed for '{short_name}'");
            return false;
        };
        let Ok(mut res) = reflect_resource.reflect_mut(world) else { return false };
        res.apply(&*new_val);
        info!("wasm_bridge: patched resource {short_name}");
        return true;
    }

    // ── Component path ─────────────────────────────────────────────────────
    if let Some(reflect_component) = type_reg.data::<ReflectComponent>() {
        let all_entities: Vec<Entity> = world.query::<Entity>().iter(world).collect();
        let all_targets: Vec<Entity> = all_entities.into_iter()
            .filter(|&id| {
                world.get_entity(id).ok()
                    .map(|eref| reflect_component.contains(eref))
                    .unwrap_or(false)
            })
            .collect();

        if all_targets.is_empty() {
            warn!("wasm_bridge: no entity has component '{base_name}'");
            return false;
        }

        // If an index was specified, patch only that entity; otherwise patch all.
        let targets: Vec<Entity> = if short_name.contains('[') {
            match all_targets.get(entity_index) {
                Some(&id) => vec![id],
                None => {
                    warn!("wasm_bridge: no entity[{entity_index}] has component '{base_name}'");
                    return false;
                }
            }
        } else {
            all_targets
        };

        let mut any_ok = false;
        for entity_id in targets {
            let merged_str = {
                let Ok(eref) = world.get_entity(entity_id) else { continue };
                let Some(cur) = reflect_component.reflect(eref) else { continue };
                let Ok(mut cur_val) = serde_json::to_value(ReflectSerializer::new(cur, registry)) else { continue };
                merge_into_inner(&mut cur_val, patch);
                let Ok(s) = serde_json::to_string(&cur_val) else { continue };
                s
            };
            let mut de = serde_json::Deserializer::from_str(&merged_str);
            let Ok(new_val) = ReflectDeserializer::new(registry).deserialize(&mut de) else {
                warn!("wasm_bridge: deserialize failed for component '{base_name}'");
                continue;
            };
            let Ok(mut ewm) = world.get_entity_mut(entity_id) else { continue };
            reflect_component.apply(&mut ewm, &*new_val);
            any_ok = true;
        }

        if any_ok { info!("wasm_bridge: patched component {base_name}"); }
        return any_ok;
    }

    warn!("wasm_bridge: '{base_name}' has neither ReflectResource nor ReflectComponent");
    false
}

/// Apply a flat JSON patch directly to a reflected struct, one field at a time.
///
/// Used for types (like StandardMaterial) that don't have `#[reflect(Deserialize)]`
/// and therefore can't be round-tripped through ReflectDeserializer.
/// Each field is deserialized individually with TypedReflectDeserializer (which uses
/// the field type's own ReflectDeserialize impl, e.g. Color, f32, bool).
/// Fields that can't be deserialized (e.g. Handle<Image>) are silently skipped.
fn apply_patch_to_struct(target: &mut dyn Reflect, patch: &Value, registry: &TypeRegistry) {
    let Value::Object(patch_map) = patch else { return };
    let ReflectMut::Struct(st) = target.reflect_mut() else { return };
    for (field_name, field_val) in patch_map {
        let type_id = st
            .field(field_name)
            .and_then(|f| f.get_represented_type_info())
            .map(|ti| ti.type_id());
        let Some(tid) = type_id else { continue };
        let Some(type_reg) = registry.get(tid) else { continue };
        let Ok(field_json) = serde_json::to_string(field_val) else { continue };
        let mut de = serde_json::Deserializer::from_str(&field_json);
        let Ok(new_field) =
            TypedReflectDeserializer::new(type_reg, registry).deserialize(&mut de)
        else {
            continue;
        };
        let Some(field_mut) = st.field_mut(field_name) else { continue };
        field_mut.apply(&*new_field);
    }
}

// ---------------------------------------------------------------------------
// Schema introspection — walks Bevy TypeInfo to expose field names & types
// ---------------------------------------------------------------------------

/// Build a JSON schema for a single registered type using its `TypeInfo`.
fn build_type_schema(info: &bevy::reflect::TypeInfo) -> Value {
    use bevy::reflect::{TypeInfo, VariantInfo};

    let name = info.type_path_table().short_path();

    match info {
        TypeInfo::Struct(si) => {
            let fields: Vec<Value> = (0..si.field_len())
                .filter_map(|i| {
                    let field = si.field_at(i)?;
                    let type_short = field
                        .type_path_table()
                        .short_path()
                        .to_string();
                    Some(serde_json::json!({
                        "name": field.name(),
                        "type": type_short,
                    }))
                })
                .collect();
            serde_json::json!({
                "name": name,
                "kind": "struct",
                "fields": fields,
            })
        }
        TypeInfo::Enum(ei) => {
            let variants: Vec<Value> = (0..ei.variant_len())
                .filter_map(|i| {
                    let variant = ei.variant_at(i)?;
                    let mut v = serde_json::json!({ "name": variant.name() });
                    if let VariantInfo::Struct(sv) = variant {
                        let fields: Vec<Value> = (0..sv.field_len())
                            .filter_map(|j| {
                                let f = sv.field_at(j)?;
                                Some(serde_json::json!({
                                    "name": f.name(),
                                    "type": f.type_path_table().short_path(),
                                }))
                            })
                            .collect();
                        if !fields.is_empty() {
                            v["fields"] = Value::Array(fields);
                        }
                    }
                    Some(v)
                })
                .collect();
            serde_json::json!({
                "name": name,
                "kind": "enum",
                "variants": variants,
            })
        }
        _ => serde_json::json!({
            "name": name,
            "kind": "opaque",
        }),
    }
}

/// Populate SCHEMA_CACHE from the Bevy type registry (once).
fn populate_schema_cache(registry: &TypeRegistry) {
    {
        let Ok(cache) = SCHEMA_CACHE.lock() else { return };
        if !cache.is_empty() {
            return; // already populated
        }
    }

    let mut entries: Vec<(String, String)> = Vec::new();

    for type_reg in registry.iter() {
        // Only include types that are resources or components
        let is_resource = type_reg.data::<bevy::ecs::reflect::ReflectResource>().is_some();
        let is_component = type_reg.data::<bevy::ecs::reflect::ReflectComponent>().is_some();
        if !is_resource && !is_component {
            continue;
        }
        let info = type_reg.type_info();
        let name = info.type_path_table().short_path().to_string();
        let schema = build_type_schema(info);
        if let Ok(json) = serde_json::to_string(&schema) {
            entries.push((name, json));
        }
    }

    // Also add StandardMaterial schema (for "Material:*" queries)
    if let Some(mat_reg) = registry
        .iter()
        .find(|r| r.type_info().type_path_table().short_path() == "StandardMaterial")
    {
        let schema = build_type_schema(mat_reg.type_info());
        if let Ok(json) = serde_json::to_string(&schema) {
            entries.push(("StandardMaterial".to_string(), json));
        }
    }

    if let Ok(mut cache) = SCHEMA_CACHE.lock() {
        *cache = entries;
    }
}

/// Read a schema from the cache. Name "*" returns all schemas as a JSON array.
fn schema_get(name: &str) -> String {
    let Ok(cache) = SCHEMA_CACHE.lock() else {
        return "null".to_string();
    };
    if name == "*" || name.is_empty() {
        let all: Vec<Value> = cache
            .iter()
            .filter_map(|(_, json)| serde_json::from_str(json).ok())
            .collect();
        serde_json::to_string(&all).unwrap_or_else(|_| "[]".to_string())
    } else {
        // "Material:X" → return StandardMaterial schema
        let lookup = if name.starts_with("Material:") {
            "StandardMaterial"
        } else {
            name
        };
        cache
            .iter()
            .find(|(k, _)| k == lookup)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "null".to_string())
    }
}

/// Merges `patch` into the inner value of the type-path-wrapped `target`.
/// `target` = `{"full::type::Path": inner}`, `patch` = fields relative to `inner`.
pub(crate) fn merge_into_inner(target: &mut Value, patch: &Value) {
    let Value::Object(patch_map) = patch else { return };
    let Value::Object(wrap) = target else { return };
    if let Some(inner) = wrap.values_mut().next() {
        merge_obj(inner, patch_map);
    }
}

pub(crate) fn merge_obj(target: &mut Value, patch: &serde_json::Map<String, Value>) {
    match target {
        Value::Object(map) => {
            for (k, v) in patch {
                match (map.get_mut(k), v) {
                    // Both sides are objects → recurse (handles nested structs/enums).
                    (Some(cur @ Value::Object(_)), Value::Object(sub)) => merge_obj(cur, sub),
                    // Otherwise replace (handles primitive fields and enum variant switches).
                    _ => { map.insert(k.clone(), v.clone()); }
                }
            }
        }
        // Target is not an object (e.g. current enum variant was a string) → replace entirely.
        _ => *target = Value::Object(patch.clone().into_iter().collect()),
    }
}

/// Keep cache fresh: serializes Resources, Components, and Materials each PostUpdate.
///
/// Component strategy: find entities that carry at least one pmetra_demo component
/// ("interesting entities"), then expose ALL reflected components on those entities
/// regardless of crate — giving us Transform, Visibility, Name, etc. for free.
///
/// Material strategy: follow MeshMaterial3d<StandardMaterial> on interesting entities
/// and cache as "Material:<pmetra_component_name>" keys.
///
/// Cache is fully rebuilt each frame — stale entries from despawned entities are
/// automatically evicted with no extra bookkeeping.
fn sync_resource_cache(world: &mut World) {
    let registry = world.get_resource::<AppTypeRegistry>().map(|r| r.clone());
    let Some(registry) = registry else { return };

    // Populate type schemas once (idempotent — skips if already filled).
    populate_schema_cache(&registry.read());

    let mut new_cache: Vec<(String, String)> = Vec::new();

    // ── Resources ─────────────────────────────────────────────────────────
    {
        let reg = registry.read();
        let resource_updates: Vec<(String, String)> = reg
            .iter()
            .filter_map(|type_reg| {
                let reflect_resource = type_reg.data::<ReflectResource>()?;
                let reflected = reflect_resource.reflect(&*world).ok()?;
                let name = type_reg.type_info().type_path_table().short_path().to_string();
                let json = serde_json::to_string(&ReflectSerializer::new(reflected, &reg)).ok()?;
                Some((name, json))
            })
            .collect();
        new_cache.extend(resource_updates);
    }

    // ── Build component type lists (registry read dropped before entity scan) ─
    // pmetra_components: used to identify interesting entities
    // all_component_types: exposed on those entities (any crate)
    let pmetra_components: Vec<(String, ReflectComponent)>;
    let all_component_types: Vec<(String, ReflectComponent)>;
    {
        let reg = registry.read();
        pmetra_components = reg
            .iter()
            .filter(|r| r.data::<ReflectResource>().is_none())
            .filter(|r| r.type_info().type_path_table().crate_name() == Some("pmetra_demo"))
            .filter_map(|r| {
                let rc = r.data::<ReflectComponent>()?.clone();
                let name = r.type_info().type_path_table().short_path().to_string();
                Some((name, rc))
            })
            .collect();
        all_component_types = reg
            .iter()
            .filter(|r| r.data::<ReflectResource>().is_none())
            .filter_map(|r| {
                let rc = r.data::<ReflectComponent>()?.clone();
                let name = r.type_info().type_path_table().short_path().to_string();
                Some((name, rc))
            })
            .collect();
    }

    // ── Find interesting entities (have at least one pmetra_demo component) ─
    let all_entity_ids: Vec<Entity> = world.query::<Entity>().iter(world).collect();
    let interesting_entities: Vec<(Entity, String)> = all_entity_ids
        .iter()
        .copied()
        .filter_map(|id| {
            let eref = world.get_entity(id).ok()?;
            let pmetra_name = pmetra_components
                .iter()
                .find(|(_, rc)| rc.contains(eref))
                .map(|(name, _)| name.clone())?;
            Some((id, pmetra_name))
        })
        .collect();

    if !interesting_entities.is_empty() {
        // ── All components on interesting entities (any crate) ─────────────
        // Exposes Transform, Visibility, Name, etc. alongside CAD params.
        //
        // For component types with multiple entities (e.g. two TowerExtensions in a
        // multi-model scene), emit indexed keys:
        //   "TowerExtension"    → entity 0  (backward-compatible, always present)
        //   "TowerExtension[1]" → entity 1
        //   "TowerExtension[2]" → entity 2, etc.
        {
            let reg = registry.read();
            for (name, reflect_component) in &all_component_types {
                let matches: Vec<Entity> = interesting_entities
                    .iter()
                    .filter_map(|(id, _)| {
                        world
                            .get_entity(*id)
                            .ok()
                            .filter(|eref| reflect_component.contains(*eref))
                            .map(|_| *id)
                    })
                    .collect();
                for (idx, &entity_id) in matches.iter().enumerate() {
                    let Ok(eref) = world.get_entity(entity_id) else { continue };
                    let Some(cur) = reflect_component.reflect(eref) else { continue };
                    let Ok(json) = serde_json::to_string(&ReflectSerializer::new(cur, &reg))
                    else {
                        continue;
                    };
                    // First entity uses the bare name; subsequent use [n] suffix.
                    let key = if idx == 0 {
                        name.clone()
                    } else {
                        format!("{name}[{idx}]")
                    };
                    new_cache.push((key, json));
                }
            }
        }

        // ── Materials on interesting entities (and their direct children) ──
        // Key format: "Material:<pmetra_component_name>[n]"
        // First entity: "Material:TowerExtension" (no suffix), subsequent: "Material:TowerExtension[1]"
        {
            let reg = registry.read();
            if let Some(assets) = world.get_resource::<Assets<StandardMaterial>>() {
                // Group by pmetra_name to track per-type index
                let mut name_counts: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for (entity_id, pmetra_name) in &interesting_entities {
                    // Check the entity itself, then its direct children
                    let candidates: Vec<Entity> = {
                        let mut v = vec![*entity_id];
                        if let Ok(eref) = world.get_entity(*entity_id) {
                            if let Some(children) = eref.get::<Children>() {
                                for child in children.iter() { v.push(child); }
                            }
                        }
                        v
                    };
                    for eid in candidates {
                        let Ok(eref) = world.get_entity(eid) else { continue };
                        let Some(mat_comp) = eref.get::<MeshMaterial3d<StandardMaterial>>() else {
                            continue;
                        };
                        let Some(material) = assets.get(mat_comp.id()) else { continue };
                        let Ok(json) = serde_json::to_string(&ReflectSerializer::new(
                            material as &dyn Reflect,
                            &reg,
                        )) else {
                            continue;
                        };
                        let idx = name_counts.entry(pmetra_name.as_str()).or_insert(0);
                        let key = if *idx == 0 {
                            format!("Material:{pmetra_name}")
                        } else {
                            format!("Material:{pmetra_name}[{idx}]")
                        };
                        *idx += 1;
                        new_cache.push((key, json));
                        break; // first material per entity is sufficient
                    }
                }
            }
        }
    }

    // Replace entire cache — automatic eviction of stale entries.
    let Ok(mut cache) = RESOURCE_CACHE.lock() else { return };
    *cache = new_cache;
}

// ---------------------------------------------------------------------------
// Plugin — the only thing plugin.rs touches
// ---------------------------------------------------------------------------

pub struct WasmBridgePlugin;

impl Plugin for WasmBridgePlugin {
    fn build(&self, app: &mut App) {
        mount_js_namespace();
        let params = read_url_params();
        // WebSocket URL: ?ws=wss://your-worker.dev overrides everything.
        // Otherwise derive from the page's hostname — so a phone loading
        // http://<mac-ip>:3000 automatically connects to ws://<mac-ip>:9001.
        let ws_url = params.get("ws").cloned().unwrap_or_else(default_ws_url);
        connect_websocket(&ws_url);
        // Restore shapes saved in localStorage from previous sessions.
        restore_persisted_shapes();
        // Apply ?model=X URL query param (e.g. ?model=ExpNurbsSolid).
        if let Some(variant) = params.get("model") {
            info!("wasm_bridge: URL ?model={variant}");
            push_cmd(BridgeCommand::Set {
                resource: "CadGeneratedModelSpawner".to_string(),
                value: serde_json::json!({ "selected_params": variant }),
                seq: None,
            });
        }
        // PreUpdate: mutations visible to all Update systems via change detection.
        // PostUpdate: cache reflects the final state of each frame.
        app.add_systems(PreUpdate, apply_bridge_commands)
            .add_systems(PostUpdate, sync_resource_cache);
    }
}

/// Derive the default WS URL from the current page's location.
///
/// All modes use the `/ws` path — clean separation from the UI root.
///
///   HTTPS (Cloudflare prod)   → wss://<host>/ws
///   HTTP port 3000 (trunk)    → ws://<host>:9001/ws  (separate MCP server)
///   HTTP port 9001 (binary)   → ws://<host>:9001/ws  (same origin)
///   HTTP port 8787 (wrangler) → ws://<host>:8787/ws  (same origin)
fn default_ws_url() -> String {
    let Ok(window) = js_sys::global().dyn_into::<web_sys::Window>() else {
        return "ws://localhost:9001/ws".to_string();
    };
    let location = window.location();
    let protocol = location.protocol().unwrap_or_default();
    let hostname = location.hostname().unwrap_or_default();
    let port = location.port().unwrap_or_default();

    let host = if hostname.is_empty() { "localhost".to_string() } else { hostname };

    match protocol.as_str() {
        // Cloudflare Workers (production) — WSS on same host.
        "https:" => format!("wss://{host}/ws"),
        _ => {
            // Port 3000 = trunk serve (dev) → MCP server on port 9001.
            // Everything else = same origin (binary on 9001, wrangler on 8787).
            let ws_port = if port == "3000" || port.is_empty() { "9001" } else { &port };
            format!("ws://{host}:{ws_port}/ws")
        }
    }
}

/// Parse URL query parameters into a key-value map.
fn read_url_params() -> std::collections::HashMap<String, String> {
    let mut params = std::collections::HashMap::new();
    let Ok(window) = js_sys::global().dyn_into::<web_sys::Window>() else { return params };
    let href = window.location().href().unwrap_or_default();
    let Some(query) = href.split('?').nth(1) else { return params };
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(key), Some(val)) = (kv.next(), kv.next()) {
            params.insert(key.to_string(), val.to_string());
        }
    }
    params
}
