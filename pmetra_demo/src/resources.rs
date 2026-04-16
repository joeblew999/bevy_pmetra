use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Resource, Reflect, Default, Serialize, Deserialize, TS)]
#[reflect(Resource)]
#[ts(export)]
pub struct CadGeneratedModelSpawner {
    pub selected_params: CadGeneratedModelParamsId,
}

#[derive(Debug, Reflect, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum CadGeneratedModelParamsId {
    SimplCubeAtCylinder,
    TowerExtension,
    RoundCabinSegment,
    MultiModelsSimplCubeAtCylinderAndTowerExtension,
    #[default]
    MultiModels2TowerExtensions,
    ExpNurbsSolid,
}
