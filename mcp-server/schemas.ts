/**
 * Resource/Component schemas — derived from live bridge introspection (2026-04-16).
 *
 * All 33 items currently exposed by wasm_bridge.rs are documented here.
 * To regenerate: run the app, open browser console, call window.pmetra.list()
 * and window.pmetra.get("<name>") for each entry.
 *
 * Naming conventions:
 *   Color fields accept either form:
 *     { LinearRgba: { red, green, blue, alpha } }   ← linear light-correct
 *     { Srgba:      { red, green, blue, alpha } }   ← sRGB (0–1 range)
 *   Duration fields: { secs: number; nanos: number }
 *   Vec3 fields: [x, y, z] as [number, number, number]
 *   Quaternion fields: [x, y, z, w] as [number, number, number, number]
 *
 * "Material:<ComponentName>" keys expose the StandardMaterial on the entity
 * that carries that pmetra CAD component (walks parent → children).
 *
 * CAD param components (TowerExtension etc.) only appear in list() when
 * the corresponding model variant is active in CadGeneratedModelSpawner.
 */

export interface ResourceSchema {
  tsType: string;
  description: string;
  /** If true, setting this value is useful. If false, it's read-only or internal. */
  writable: boolean;
}

// ─── Shared sub-types ─────────────────────────────────────────────────────────

const COLOR_LINEAR = `{ LinearRgba: { red: number; green: number; blue: number; alpha: number } }`;
const COLOR_SRGB   = `{ Srgba: { red: number; green: number; blue: number; alpha: number } }`;
const COLOR_ANY    = `${COLOR_LINEAR} | ${COLOR_SRGB}`;
const DURATION     = `{ secs: number; nanos: number }`;
const VEC3         = `[number, number, number]`;
const QUAT         = `[number, number, number, number]`;

// ─── Schema map ───────────────────────────────────────────────────────────────

export const RESOURCE_SCHEMAS: Record<string, ResourceSchema> = {

  // ── CAD model control ──────────────────────────────────────────────────────

  CadGeneratedModelSpawner: {
    tsType: `{
  selected_params:
    | "SimplCubeAtCylinder"
    | "TowerExtension"
    | "RoundCabinSegment"
    | "MultiModelsSimplCubeAtCylinderAndTowerExtension"
    | "MultiModels2TowerExtensions"
    | "ExpNurbsSolid"
}`,
    description:
      "Selects which CAD model variant to spawn. Changing this despawns the current model and builds the new one from default parameters.",
    writable: true,
  },

  // ── CAD param components (present in list() only when that variant is active) ─

  TowerExtension: {
    tsType: `{
  tower_length: number;                  // overall height of the tower
  straight_beam_l_sect_side_len: number; // L-section side length of vertical beams
  straight_beam_l_sect_thickness: number;
  cross_beam_l_sect_side_len: number;    // L-section side length of diagonal beams
  cross_beam_l_sect_thickness: number;
  enclosure_profile_width: number;       // width of the rectangular enclosure frame
  enclosure_profile_depth: number;       // depth of the rectangular enclosure frame
}`,
    description:
      "Parameters for the TowerExtension CAD model. All dimensions in metres. Changing any field triggers an immediate geometry rebuild.",
    writable: true,
  },

  SimpleCubeAtCylinder: {
    tsType: `{
  cylinder_radius: number;    // radius of the flat cylinder base
  cylinder_height: number;    // height/thickness of the cylinder
  cube_attach_angle: number;  // angle (radians) at which cubes are attached around the rim
  cube_side_length: number;   // side length of each attached cube
}`,
    description:
      "Parameters for the SimpleCubeAtCylinder CAD model. Changing any field triggers an immediate geometry rebuild.",
    writable: true,
  },

  ExpNurbs: {
    tsType: `{
  control_point_spacing: number;
  surface_length: number;
  surface_thickness: number;
  control_points: [
    [number, number, number], // row 0 — 4 points
    [number, number, number],
    [number, number, number],
    [number, number, number],
    [number, number, number], // row 1 — 4 points
    [number, number, number],
    [number, number, number],
    [number, number, number],
  ];
}`,
    description:
      "Parameters for the ExpNurbsSolid model. 8 DVec3 control points define the NURBS surface shape. Sculpt by adjusting Y components of individual control points.",
    writable: true,
  },

  RoundCabinSegment: {
    tsType: `{
  length: number;
  radius: number;
  tolerance: number;
  window: {
    width: number;
    height: number;
    corner_radius: number;
    thickness: number;
  };
}`,
    description:
      "Parameters for the RoundCabinSegment model. 'window' is a nested RoundRectCuboid — patch it in a single set() call.",
    writable: true,
  },

  // ── Material (per CAD entity, key = "Material:<ComponentName>") ────────────

  "Material:TowerExtension": {
    tsType: `{
  base_color?: ${COLOR_ANY};
  emissive?: { red: number; green: number; blue: number; alpha: number };
  perceptual_roughness?: number;    // 0.0 = mirror, 1.0 = fully rough
  metallic?: number;                // 0.0 = dielectric, 1.0 = metal
  reflectance?: number;             // Fresnel reflectance at normal incidence
  diffuse_transmission?: number;
  specular_transmission?: number;
  ior?: number;                     // index of refraction (default 1.5)
  double_sided?: boolean;
  unlit?: boolean;
  fog_enabled?: boolean;
  alpha_mode?: "Opaque" | "Blend" | "Premultiplied" | "Add" | "Multiply";
  depth_bias?: number;
}`,
    description:
      "StandardMaterial on the entity carrying TowerExtension. Texture fields (base_color_texture etc.) are always null in pmetra — omit them. Set using 'Material:TowerExtension' as the resource name.",
    writable: true,
  },

  "Material:SimpleCubeAtCylinder": {
    tsType: `{ base_color?: ${COLOR_ANY}; perceptual_roughness?: number; metallic?: number; unlit?: boolean; alpha_mode?: string }`,
    description: "StandardMaterial on the entity carrying SimpleCubeAtCylinder.",
    writable: true,
  },

  "Material:ExpNurbs": {
    tsType: `{ base_color?: ${COLOR_ANY}; perceptual_roughness?: number; metallic?: number; unlit?: boolean; alpha_mode?: string }`,
    description: "StandardMaterial on the entity carrying ExpNurbs.",
    writable: true,
  },

  "Material:RoundCabinSegment": {
    tsType: `{ base_color?: ${COLOR_ANY}; perceptual_roughness?: number; metallic?: number; unlit?: boolean; alpha_mode?: string }`,
    description: "StandardMaterial on the entity carrying RoundCabinSegment.",
    writable: true,
  },

  // ── Scene globals ──────────────────────────────────────────────────────────

  GlobalAmbientLight: {
    tsType: `{
  color?: ${COLOR_ANY};
  brightness?: number;                   // luminance in lux, e.g. 400 = dim, 2000 = bright
  affects_lightmapped_meshes?: boolean;
}`,
    description: "Global ambient light applied to the entire scene.",
    writable: true,
  },

  ClearColor: {
    tsType: `${COLOR_ANY}`,
    description: "Background clear color of the viewport.",
    writable: true,
  },

  // ── Transform (on the pmetra CAD root entity) ──────────────────────────────

  Transform: {
    tsType: `{
  translation?: ${VEC3};   // world position [x, y, z]
  rotation?: ${QUAT};      // quaternion [x, y, z, w], identity = [0,0,0,1]
  scale?: ${VEC3};         // scale per axis, uniform = [s,s,s]
}`,
    description:
      "Position/rotation/scale of the CAD model root entity. Changing translation moves the model; changing scale uniformly resizes it.",
    writable: true,
  },

  Visibility: {
    tsType: `"Inherited" | "Visible" | "Hidden"`,
    description: "Visibility of the CAD model root entity. Use 'Hidden' to hide, 'Visible' to force-show regardless of parent.",
    writable: true,
  },

  // ── Simulation time ────────────────────────────────────────────────────────

  "Time<Virtual>": {
    tsType: `{
  context?: {
    paused?: boolean;
    relative_speed?: number;   // 1.0 = normal, 0.5 = half speed, 2.0 = double
    max_delta?: ${DURATION};
  };
}`,
    description:
      "Virtual simulation time. Set context.paused=true to freeze physics/animation; context.relative_speed to slow or speed up simulation.",
    writable: true,
  },

  // ── Physics debug ──────────────────────────────────────────────────────────

  DebugRenderContext: {
    tsType: `{
  enabled: boolean;
  default_collider_debug?: "AlwaysRender" | "NeverRender";
}`,
    description: "Toggles Rapier physics debug wireframe rendering over all colliders.",
    writable: true,
  },

  // ── Renderer ───────────────────────────────────────────────────────────────

  DefaultOpaqueRendererMethod: {
    tsType: `"Forward" | "Deferred"`,
    description:
      "Opaque render method. 'Deferred' is silently ignored on WebGPU/browser (no GBuffer support) — stays 'Forward'.",
    writable: true,
  },

  // ── Shadow maps ────────────────────────────────────────────────────────────

  DirectionalLightShadowMap: {
    tsType: `{ size: number }`,
    description: "Shadow map resolution for directional lights (power of 2: 512, 1024, 2048, 4096).",
    writable: true,
  },

  PointLightShadowMap: {
    tsType: `{ size: number }`,
    description: "Shadow map resolution for point lights.",
    writable: true,
  },

  // ── Audio ──────────────────────────────────────────────────────────────────

  GlobalVolume: {
    tsType: `{ volume: { Linear: number } }`,
    description: "Master audio volume. Linear(0.0) = silent, Linear(1.0) = full.",
    writable: true,
  },

  DefaultSpatialScale: {
    tsType: `${VEC3}`,
    description: "Scale factor applied to 3D audio spatial positions.",
    writable: true,
  },

  // ── Interaction / picking ──────────────────────────────────────────────────

  PickingSettings: {
    tsType: `{
  is_enabled?: boolean;
  is_input_enabled?: boolean;
  is_hover_enabled?: boolean;
  is_window_picking_enabled?: boolean;
}`,
    description: "Global picking/interaction settings. Disable is_enabled to stop all mouse interaction.",
    writable: true,
  },

  PointerInputSettings: {
    tsType: `{ is_touch_enabled?: boolean; is_mouse_enabled?: boolean }`,
    description: "Enable/disable mouse or touch pointer input.",
    writable: true,
  },

  MeshPickingSettings: {
    tsType: `{
  require_markers?: boolean;
  ray_cast_visibility?: "VisibleInView" | "Any";
}`,
    description: "Settings for mesh-based raycasting used by the picking system.",
    writable: true,
  },

  UiPickingSettings: {
    tsType: `{ require_markers?: boolean }`,
    description: "Whether UI elements require explicit PickingBehavior markers.",
    writable: true,
  },

  SpritePickingSettings: {
    tsType: `{
  require_markers?: boolean;
  picking_mode?: { AlphaThreshold: number } | "BoundingBox";
}`,
    description: "Picking mode for sprites.",
    writable: true,
  },

  UiScale: {
    tsType: `number`,
    description: "Global UI scale multiplier (1.0 = default).",
    writable: true,
  },

  // ── Read-only / internal (present in list, not useful to set) ─────────────

  "Time<()>": {
    tsType: `{ delta_secs: number; elapsed_secs: number; elapsed_secs_wrapped: number }`,
    description: "Real wall-clock time. Read-only — use Time<Virtual> to control simulation speed.",
    writable: false,
  },

  "Time<Fixed>": {
    tsType: `{ context: { timestep: ${DURATION}; overstep: ${DURATION} }; delta_secs: number; elapsed_secs: number }`,
    description: "Fixed-timestep time. Read-only.",
    writable: false,
  },

  GlobalTransform: {
    tsType: `[number, number, number, number, number, number, number, number, number, number, number, number]`,
    description: "World-space affine transform matrix (column-major 3x4). Computed from Transform — do not set directly.",
    writable: false,
  },

  InheritedVisibility: {
    tsType: `boolean`,
    description: "Computed visibility propagated from parent. Read-only — set Visibility instead.",
    writable: false,
  },

  ViewVisibility: {
    tsType: `number`,
    description: "Bitmask indicating which views can see this entity. Read-only — computed by Bevy.",
    writable: false,
  },

  Children: {
    tsType: `number[]`,
    description: "Entity IDs of direct child entities. Read-only — opaque ECS internals.",
    writable: false,
  },

  ObservedBy: {
    tsType: `number[]`,
    description: "Entity IDs of observers watching this entity. Read-only — ECS internals.",
    writable: false,
  },

  TransformTreeChanged: {
    tsType: `{}`,
    description: "Marker component set when the transform hierarchy changes. Read-only.",
    writable: false,
  },

  GizmoConfigStore: {
    tsType: `{}`,
    description: "Gizmo rendering configuration. Opaque — cannot be patched via reflection.",
    writable: false,
  },

  TilemapChunkMeshCache: {
    tsType: `{}`,
    description: "Internal tilemap mesh cache. Read-only.",
    writable: false,
  },

  ManageAccessibilityUpdates: {
    tsType: `boolean`,
    description: "Whether Bevy manages accessibility tree updates. Read-only in practice.",
    writable: false,
  },

  AccumulatedMouseScroll: {
    tsType: `{ unit: "Line" | "Pixel"; delta: [number, number] }`,
    description: "Mouse scroll accumulated this frame. Read-only input state.",
    writable: false,
  },

  AccumulatedMouseMotion: {
    tsType: `{ delta: [number, number] }`,
    description: "Mouse motion accumulated this frame. Read-only input state.",
    writable: false,
  },
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Returns the schema for a resource, or undefined if unknown. */
export function getSchema(resourceName: string): ResourceSchema | undefined {
  return RESOURCE_SCHEMAS[resourceName];
}

/** Returns all known resource names. */
export function knownResources(): string[] {
  return Object.keys(RESOURCE_SCHEMAS);
}

/** Returns only the writable resources — useful for building MCP tool descriptions. */
export function writableResources(): string[] {
  return Object.entries(RESOURCE_SCHEMAS)
    .filter(([, s]) => s.writable)
    .map(([name]) => name);
}
