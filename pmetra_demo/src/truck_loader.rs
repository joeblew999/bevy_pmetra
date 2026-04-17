//! Truck CAD file loader.
//!
//! Loads Truck JSON and STEP files, tessellates B-rep geometry into Bevy meshes,
//! and serializes back. Used by the WASM bridge for AI-driven CAD operations.
//!
//! **JSON files** use `truck_modeling` types and support full round-trip:
//! load → edit shell → re-tessellate → save.
//!
//! **STEP files** use `truck_stepio` types (different curve/surface enums) and are
//! currently view-only: load → tessellate → render. The raw STEP string is stored
//! for re-export.

use bevy::prelude::*;
use bevy_pmetra::{
    prelude::*,
    re_exports::{
        anyhow::{self, Context, Result},
        truck_meshalgo::tessellation::{MeshableShape, MeshedShape},
        truck_modeling,
    },
};

// Re-export for use in bridge queries
pub use truck_meshalgo_filters::OptimizingFilter;
mod truck_meshalgo_filters {
    pub use bevy_pmetra::re_exports::truck_meshalgo::filters::OptimizingFilter;
}

/// Component marking an entity loaded from a Truck JSON file.
///
/// Holds the live B-rep shell for editing and re-tessellation.
#[derive(Component)]
pub struct TruckModel {
    /// Display name or file stem.
    pub name: String,
    /// The live B-rep shell (editable, re-tessellatable, JSON-serializable).
    pub shell: truck_modeling::Shell,
    /// True if the original JSON was a CompressedSolid `{boundaries:[...]}`.
    /// Used by save to wrap back in Solid format for round-trip fidelity.
    pub was_solid: bool,
}

/// Component marking an entity loaded from a STEP file.
///
/// STEP geometry uses different curve/surface types than `truck_modeling`,
/// so editing is not yet supported. The raw STEP string is kept for re-export.
#[derive(Component)]
pub struct StepModel {
    /// Display name or file stem.
    pub name: String,
    /// Original STEP file content (for re-export / save).
    pub step_data: String,
}

/// Default tessellation tolerance.
const DEFAULT_TOL: f64 = 0.01;

// ---------------------------------------------------------------------------
// JSON — full round-trip: load, edit, re-tessellate, save
// ---------------------------------------------------------------------------

/// Parsed Truck JSON result — carries format info for round-trip saving.
pub struct ParsedTruckJson {
    pub shells: Vec<truck_modeling::Shell>,
    /// True if the input was a CompressedSolid `{boundaries:[...]}`.
    pub was_solid: bool,
}

/// Deserialize a Truck JSON string into one or more Shells.
///
/// Supports two formats:
/// - **CompressedShell** `{vertices, edges, faces}` → single Shell
/// - **CompressedSolid** `{boundaries: [CompressedShell, ...]}` → one Shell per boundary
pub fn parse_truck_json(json: &str) -> Result<ParsedTruckJson> {
    // Try Shell (CompressedShell) first
    if let Ok(shell) = serde_json::from_str::<truck_modeling::Shell>(json) {
        return Ok(ParsedTruckJson { shells: vec![shell], was_solid: false });
    }
    // Try Solid (CompressedSolid) — has {boundaries: [...]}
    if let Ok(solid) = serde_json::from_str::<truck_modeling::Solid>(json) {
        let shells: Vec<_> = solid.into_boundaries();
        if shells.is_empty() {
            anyhow::bail!("Truck JSON parsed as Solid but contained no boundaries");
        }
        return Ok(ParsedTruckJson { shells, was_solid: true });
    }
    anyhow::bail!("failed to parse Truck JSON as Shell or Solid")
}

/// Serialize a Shell back to Truck JSON (pretty-printed).
pub fn shell_to_json(shell: &truck_modeling::Shell) -> Result<String> {
    serde_json::to_string_pretty(shell).context("failed to serialize Shell to JSON")
}

/// Tessellate a truck Shell into a Bevy [`Mesh`].
pub fn tessellate_shell(shell: &truck_modeling::Shell, tol: f64) -> Result<Mesh> {
    let cad_shell = CadShell {
        shell: shell.clone(),
        tagged_elements: CadTaggedElements::default(),
    };
    let polygon = cad_shell.build_polygon_with_tol(tol)?;
    Ok(BevyMeshBuilder::from(&polygon).into())
}

/// Load a Truck JSON string into the world as rendered mesh entity/entities.
///
/// Supports both CompressedShell (single shell) and CompressedSolid (multi-shell).
/// Returns the first spawned entity (for single-shell cases) or all of them.
pub fn spawn_from_json(
    world: &mut World,
    name: &str,
    json: &str,
    transform: Transform,
) -> Result<Entity> {
    let parsed = parse_truck_json(json)?;
    let material = default_material();
    let mut first_entity = None;

    let shell_count = parsed.shells.len();
    for (i, shell) in parsed.shells.iter().enumerate() {
        let mesh = tessellate_shell(shell, DEFAULT_TOL)?;
        let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
        let mat_handle = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(material.clone());

        let entity = world
            .spawn((
                Mesh3d(mesh_handle),
                MeshMaterial3d(mat_handle),
                transform,
                TruckModel {
                    name: if shell_count == 1 { name.to_string() } else { format!("{name}_{i}") },
                    shell: shell.clone(),
                    was_solid: parsed.was_solid,
                },
            ))
            .id();

        if first_entity.is_none() {
            first_entity = Some(entity);
        }
        info!("truck_loader: spawned JSON '{}' shell {} as {:?}", name, i, entity);
    }

    first_entity.context("no shells to spawn")
}

/// Re-tessellate a [`TruckModel`] entity after geometry edits.
pub fn retessellate(world: &mut World, entity: Entity) -> Result<()> {
    let (shell, handle_id) = {
        let e = world.entity(entity);
        let model = e.get::<TruckModel>().context("entity has no TruckModel")?;
        let mesh3d = e.get::<Mesh3d>().context("entity has no Mesh3d")?;
        (model.shell.clone(), mesh3d.0.id())
    };

    let new_mesh = tessellate_shell(&shell, DEFAULT_TOL)?;
    let _ = world
        .resource_mut::<Assets<Mesh>>()
        .insert(handle_id, new_mesh);

    Ok(())
}

/// Save a [`TruckModel`] entity's shell back to JSON.
///
/// If the original was loaded as a Solid (multiple boundaries), wraps the shell
/// back in a Solid so the output format matches the input format.
pub fn save_entity_json(world: &World, entity: Entity) -> Result<String> {
    let model = world
        .entity(entity)
        .get::<TruckModel>()
        .context("entity has no TruckModel")?;
    if model.was_solid {
        // Original was CompressedSolid — wrap back in Solid for round-trip fidelity.
        let solid = truck_modeling::Solid::new(vec![model.shell.clone()]);
        serde_json::to_string_pretty(&solid).context("failed to serialize Solid to JSON")
    } else {
        // Original was a standalone CompressedShell.
        shell_to_json(&model.shell)
    }
}

// ---------------------------------------------------------------------------
// STEP — load and render (view-only, keeps raw STEP for re-export)
// ---------------------------------------------------------------------------

/// Load a STEP file string, tessellate all geometry, and spawn mesh entities.
///
/// STEP geometry uses `Curve3D`/`Surface` types from truck_stepio which differ
/// from truck_modeling types. Models are spawned with a [`StepModel`] component
/// that stores the raw STEP data for re-export.
///
/// Returns all spawned entities.
pub fn spawn_from_step(
    world: &mut World,
    name: &str,
    step_str: &str,
    transform: Transform,
) -> Result<Vec<Entity>> {
    use truck_stepio::r#in::Table;

    let table =
        Table::from_step(step_str).context("failed to parse STEP file")?;

    let mut entities = Vec::new();
    let material = default_material();
    let step_data = step_str.to_string();

    // Manifold solid breps → tessellate CompressedSolid directly
    for (_id, holder) in &table.manifold_solid_brep {
        match table
            .to_compressed_solid(holder)
            .map_err(|e| anyhow::anyhow!("{e}"))
        {
            Ok(compressed) => {
                let meshed = compressed.triangulation(DEFAULT_TOL);
                let mut polygon = meshed.to_polygon();
                polygon.remove_degenerate_faces().remove_unused_attrs();
                let mesh: Mesh = BevyMeshBuilder::from(&polygon).into();

                let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
                let mat_handle = world
                    .resource_mut::<Assets<StandardMaterial>>()
                    .add(material.clone());
                let entity = world
                    .spawn((
                        Mesh3d(mesh_handle),
                        MeshMaterial3d(mat_handle),
                        transform,
                        StepModel {
                            name: name.to_string(),
                            step_data: step_data.clone(),
                        },
                    ))
                    .id();
                entities.push(entity);
            }
            Err(e) => warn!("truck_loader: STEP solid error: {e}"),
        }
    }

    // Shell-based surface models → tessellate each CompressedShell
    for (_id, holder) in &table.shell_based_surface_model {
        match table
            .to_compressed_shells(holder)
            .map_err(|e| anyhow::anyhow!("{e}"))
        {
            Ok(shells) => {
                for compressed in &shells {
                    let meshed = compressed.triangulation(DEFAULT_TOL);
                    let mut polygon = meshed.to_polygon();
                    polygon.remove_degenerate_faces().remove_unused_attrs();
                    let mesh: Mesh = BevyMeshBuilder::from(&polygon).into();

                    let mesh_handle = world.resource_mut::<Assets<Mesh>>().add(mesh);
                    let mat_handle = world
                        .resource_mut::<Assets<StandardMaterial>>()
                        .add(material.clone());
                    let entity = world
                        .spawn((
                            Mesh3d(mesh_handle),
                            MeshMaterial3d(mat_handle),
                            transform,
                            StepModel {
                                name: name.to_string(),
                                step_data: step_data.clone(),
                            },
                        ))
                        .id();
                    entities.push(entity);
                }
            }
            Err(e) => warn!("truck_loader: STEP shell error: {e}"),
        }
    }

    if entities.is_empty() {
        anyhow::bail!(
            "STEP file '{}' contained no extractable geometry",
            name
        );
    }

    info!(
        "truck_loader: spawned {} meshes from STEP '{}'",
        entities.len(),
        name
    );
    Ok(entities)
}

/// Save a [`StepModel`] entity's original STEP data.
pub fn save_entity_step(world: &World, entity: Entity) -> Result<String> {
    let model = world
        .entity(entity)
        .get::<StepModel>()
        .context("entity has no StepModel")?;
    Ok(model.step_data.clone())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn default_material() -> StandardMaterial {
    StandardMaterial {
        base_color: Color::srgb(0.7, 0.7, 0.8),
        metallic: 0.3,
        perceptual_roughness: 0.5,
        ..default()
    }
}
