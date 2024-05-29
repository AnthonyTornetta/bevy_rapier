use crate::prelude::PhysicsWorld;

pub use self::configuration::{RapierConfiguration, SimulationToRenderTime, TimestepMode};
pub use self::context::RapierContext;
pub use self::plugin::{
    NoUserData, PhysicsSet, RapierPhysicsPlugin, RapierTransformPropagateSet, RapierWorld, WorldId,
    DEFAULT_WORLD_ID,
};

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub mod systems;
mod systems_old;

mod configuration;
pub(crate) mod context;
mod narrow_phase;
#[allow(clippy::module_inception)]
pub(crate) mod plugin;

pub(crate) fn get_world<'a>(
    world_within: Option<&'a PhysicsWorld>,
    context: &'a mut RapierContext,
) -> &'a mut RapierWorld {
    let world_id = world_within.map(|x| x.world_id).unwrap_or(DEFAULT_WORLD_ID);

    context
        .get_world_mut(world_id)
        .expect("World {world_id} does not exist")
}
