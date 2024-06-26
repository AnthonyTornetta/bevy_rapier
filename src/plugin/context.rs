use bevy::prelude::*;
use core::fmt;
use std::collections::HashMap;
use std::sync::RwLock;

use crate::geometry::{Collider, PointProjection, RayIntersection};
use crate::math::{Rot, Vect};
use crate::pipeline::{CollisionEvent, ContactForceEvent, QueryFilter};
use crate::prelude::events::EventQueue;
use rapier::control::CharacterAutostep;
use rapier::prelude::{
    CCDSolver, ColliderHandle, ColliderSet, EventHandler, FeatureId, ImpulseJointHandle,
    ImpulseJointSet, IntegrationParameters, IslandManager, MultibodyJointHandle, MultibodyJointSet,
    NarrowPhase, PhysicsHooks, PhysicsPipeline, QueryFilter as RapierQueryFilter, QueryPipeline,
    Ray, Real, RigidBodyHandle, RigidBodySet,
};

use crate::geometry::ShapeCastHit;
use bevy::prelude::{Entity, EventWriter, GlobalTransform, Query};

use crate::control::{CharacterCollision, MoveShapeOptions, MoveShapeOutput};
use crate::dynamics::TransformInterpolation;
use crate::parry::query::details::ShapeCastOptions;
use crate::plugin::configuration::{SimulationToRenderTime, TimestepMode};
use crate::prelude::{CollisionGroups, RapierRigidBodyHandle};
use rapier::geometry::DefaultBroadPhase;

/// Points to the [`RapierWorld`] within the [`RapierContext`].
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Reflect)]
pub struct WorldId(pub usize);

impl std::fmt::Display for WorldId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(format!("{}", self.0).as_str())
    }
}

impl WorldId {
    /// This references a [`RapierWorld`] within the [`RapierContext`]
    ///
    /// This id is not checked for validity.
    pub fn new(id: usize) -> Self {
        Self(id)
    }
}

/// This world id is the default world that is created when the physics plugin is initialized.
///
/// This world CAN be removed from the simulation if [`RapierContext::remove_world`] is called with this ID,
/// so it may not always be valid.
pub const DEFAULT_WORLD_ID: WorldId = WorldId(0);

/// The Rapier context, containing all the state of the physics engine.
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
pub struct RapierWorld {
    /// Specifying the gravity of the physics simulation.
    pub gravity: Vect,
    /// The island manager, which detects what object is sleeping
    /// (not moving much) to reduce computations.
    pub islands: IslandManager,
    /// The broad-phase, which detects potential contact pairs.
    pub broad_phase: DefaultBroadPhase,
    /// The narrow-phase, which computes contact points, tests intersections,
    /// and maintain the contact and intersection graphs.
    pub narrow_phase: NarrowPhase,
    /// The set of rigid-bodies part of the simulation.
    pub bodies: RigidBodySet,
    /// The set of colliders part of the simulation.
    pub colliders: ColliderSet,
    /// The set of impulse joints part of the simulation.
    pub impulse_joints: ImpulseJointSet,
    /// The set of multibody joints part of the simulation.
    pub multibody_joints: MultibodyJointSet,
    /// The solver, which handles Continuous Collision Detection (CCD).
    pub ccd_solver: CCDSolver,
    /// The physics pipeline, which advance the simulation step by step.
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub pipeline: PhysicsPipeline,
    /// The query pipeline, which performs scene queries (ray-casting, point projection, etc.)
    pub query_pipeline: QueryPipeline,
    /// The integration parameters, controlling various low-level coefficient of the simulation.
    pub integration_parameters: IntegrationParameters,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) event_handler: Option<Box<dyn EventHandler>>,
    // For transform change detection.
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) last_body_transform_set: HashMap<RigidBodyHandle, GlobalTransform>,
    // NOTE: these maps are needed to handle despawning.
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) entity2body: HashMap<Entity, RigidBodyHandle>,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) entity2collider: HashMap<Entity, ColliderHandle>,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) entity2impulse_joint: HashMap<Entity, ImpulseJointHandle>,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) entity2multibody_joint: HashMap<Entity, MultibodyJointHandle>,
    // This maps the handles of colliders that have been deleted since the last
    // physics update, to the entity they was attached to.
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) deleted_colliders: HashMap<ColliderHandle, Entity>,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) collision_events_to_send: RwLock<Vec<CollisionEvent>>,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) contact_force_events_to_send: RwLock<Vec<ContactForceEvent>>,
    #[cfg_attr(feature = "serde-serialize", serde(skip))]
    pub(crate) character_collisions_collector: Vec<rapier::control::CharacterCollision>,
}

impl Default for RapierWorld {
    fn default() -> Self {
        Self {
            islands: IslandManager::new(),
            broad_phase: DefaultBroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            ccd_solver: CCDSolver::new(),
            pipeline: PhysicsPipeline::new(),
            query_pipeline: QueryPipeline::new(),
            integration_parameters: IntegrationParameters::default(),
            event_handler: None,
            last_body_transform_set: HashMap::new(),
            entity2body: HashMap::new(),
            entity2collider: HashMap::new(),
            entity2impulse_joint: HashMap::new(),
            entity2multibody_joint: HashMap::new(),
            deleted_colliders: HashMap::new(),
            character_collisions_collector: vec![],
            collision_events_to_send: RwLock::new(Vec::new()),
            contact_force_events_to_send: RwLock::new(Vec::new()),
            gravity: Vect::Y * -9.81,
        }
    }
}

impl RapierWorld {
    /// Generates bevy events for any physics interactions that have happened
    /// that are stored in the events list
    pub fn send_bevy_events(
        &mut self,
        collision_event_writer: &mut EventWriter<CollisionEvent>,
        contact_force_event_writer: &mut EventWriter<ContactForceEvent>,
    ) {
        if let Ok(mut collision_events_to_send) = self.collision_events_to_send.write() {
            for collision_event in collision_events_to_send.iter() {
                collision_event_writer.send(*collision_event);
            }

            collision_events_to_send.clear();
        }

        if let Ok(mut contact_force_events_to_send) = self.contact_force_events_to_send.write() {
            for contact_force_event in contact_force_events_to_send.iter() {
                contact_force_event_writer.send(*contact_force_event);
            }

            contact_force_events_to_send.clear();
        }
    }

    /// Sets the gravity of this world with respect to its integration parameters.
    ///
    /// Prefer using this over setting gravity manually
    pub fn set_gravity(&mut self, gravity: Vect) {
        self.gravity = gravity * self.integration_parameters.length_unit;
    }

    /// Sets the gravity of this world with respect to its integration parameters.
    ///
    /// Prefer using this over setting gravity manually
    pub fn with_gravity(mut self, gravity: Vect) -> Self {
        self.set_gravity(gravity);

        self
    }

    /// If the collider attached to `entity` is attached to a rigid-body, this
    /// returns the `Entity` containing that rigid-body.
    pub fn collider_parent(&self, entity: Entity) -> Option<Entity> {
        self.entity2collider
            .get(&entity)
            .and_then(|h| self.colliders.get(*h))
            .and_then(|co| co.parent())
            .and_then(|h| self.rigid_body_entity(h))
    }

    /// If entity is a rigid-body, this returns the collider `Entity`s attached
    /// to that rigid-body.
    pub fn rigid_body_colliders(&self, entity: Entity) -> impl Iterator<Item = Entity> + '_ {
        self.entity2body
            .get(&entity)
            .and_then(|handle| self.bodies.get(*handle))
            .map(|body| {
                body.colliders()
                    .iter()
                    .filter_map(|handle| self.collider_entity(*handle))
            })
            .into_iter()
            .flatten()
    }

    /// Retrieve the Bevy entity the given Rapier collider (identified by its handle) is attached.
    pub fn collider_entity(&self, handle: ColliderHandle) -> Option<Entity> {
        Self::collider_entity_with_set(&self.colliders, handle)
    }

    // Mostly used to avoid borrowing self completely.
    pub(crate) fn collider_entity_with_set(
        colliders: &ColliderSet,
        handle: ColliderHandle,
    ) -> Option<Entity> {
        colliders
            .get(handle)
            .map(|c| Entity::from_bits(c.user_data as u64))
    }

    /// Retrieve the Bevy entity the given Rapier rigid-body (identified by its handle) is attached.
    pub fn rigid_body_entity(&self, handle: RigidBodyHandle) -> Option<Entity> {
        self.bodies
            .get(handle)
            .map(|c| Entity::from_bits(c.user_data as u64))
    }

    /// Calls the closure `f` once after converting the given [`QueryFilter`] into a raw `rapier::QueryFilter`.
    pub fn with_query_filter<T>(
        &self,
        filter: QueryFilter,
        f: impl FnOnce(RapierQueryFilter) -> T,
    ) -> T {
        Self::with_query_filter_elts(
            &self.entity2collider,
            &self.entity2body,
            &self.colliders,
            filter,
            f,
        )
    }

    /// Without borrowing the [`RapierContext`], calls the closure `f` once
    /// after converting the given [`QueryFilter`] into a raw `rapier::QueryFilter`.
    pub fn with_query_filter_elts<T>(
        entity2collider: &HashMap<Entity, ColliderHandle>,
        entity2body: &HashMap<Entity, RigidBodyHandle>,
        colliders: &ColliderSet,
        filter: QueryFilter,
        f: impl FnOnce(RapierQueryFilter) -> T,
    ) -> T {
        let mut rapier_filter = RapierQueryFilter {
            flags: filter.flags,
            groups: filter.groups.map(CollisionGroups::into),
            exclude_collider: filter
                .exclude_collider
                .and_then(|c| entity2collider.get(&c).copied()),
            exclude_rigid_body: filter
                .exclude_rigid_body
                .and_then(|b| entity2body.get(&b).copied()),
            predicate: None,
        };

        if let Some(predicate) = filter.predicate {
            let wrapped_predicate = |h: ColliderHandle, _: &rapier::geometry::Collider| {
                Self::collider_entity_with_set(colliders, h)
                    .map(predicate)
                    .unwrap_or(false)
            };
            rapier_filter.predicate = Some(&wrapped_predicate);
            f(rapier_filter)
        } else {
            f(rapier_filter)
        }
    }

    /// Advance the simulation, based on the given timestep mode.
    #[allow(clippy::too_many_arguments)]
    pub fn step_simulation(
        &mut self,
        world_id: WorldId,
        timestep_mode: TimestepMode,
        create_bevy_events: bool,
        hooks: &dyn PhysicsHooks,
        time: &Time,
        sim_to_render_time: &mut SimulationToRenderTime,
        interpolation_query: &mut Option<
            &mut Query<(&RapierRigidBodyHandle, &mut TransformInterpolation)>,
        >,
    ) {
        let gravity = self.gravity;

        let event_queue = if create_bevy_events {
            Some(EventQueue {
                world_id,
                deleted_colliders: &self.deleted_colliders,
                collision_events: &mut self.collision_events_to_send,
                contact_force_events: &mut self.contact_force_events_to_send,
            })
        } else {
            None
        };

        let events = self
            .event_handler
            .as_deref()
            .or_else(|| event_queue.as_ref().map(|q| q as &dyn EventHandler))
            .unwrap_or(&() as &dyn EventHandler);

        match timestep_mode {
            TimestepMode::Interpolated {
                dt,
                time_scale,
                substeps,
            } => {
                self.integration_parameters.dt = dt;

                sim_to_render_time.diff += time.delta_seconds();

                while sim_to_render_time.diff > 0.0 {
                    // NOTE: in this comparison we do the same computations we
                    // will do for the next `while` iteration test, to make sure we
                    // don't get bit by potential float inaccuracy.
                    if sim_to_render_time.diff - dt <= 0.0 {
                        if let Some(interpolation_query) = interpolation_query.as_mut() {
                            // This is the last simulation step to be executed in the loop
                            // Update the previous state transforms
                            for (handle, mut interpolation) in interpolation_query.iter_mut() {
                                if let Some(body) = self.bodies.get(handle.0) {
                                    interpolation.start = Some(*body.position());
                                    interpolation.end = None;
                                }
                            }
                        }
                    }

                    let mut substep_integration_parameters = self.integration_parameters;
                    substep_integration_parameters.dt = dt / (substeps as Real) * time_scale;

                    for _ in 0..substeps {
                        self.pipeline.step(
                            &gravity.into(),
                            &substep_integration_parameters,
                            &mut self.islands,
                            &mut self.broad_phase,
                            &mut self.narrow_phase,
                            &mut self.bodies,
                            &mut self.colliders,
                            &mut self.impulse_joints,
                            &mut self.multibody_joints,
                            &mut self.ccd_solver,
                            None,
                            hooks,
                            events,
                        );
                    }

                    sim_to_render_time.diff -= dt;
                }
            }
            TimestepMode::Variable {
                max_dt,
                time_scale,
                substeps,
            } => {
                self.integration_parameters.dt = (time.delta_seconds() * time_scale).min(max_dt);

                let mut substep_integration_parameters = self.integration_parameters;
                substep_integration_parameters.dt /= substeps as Real;

                for _ in 0..substeps {
                    self.pipeline.step(
                        &gravity.into(),
                        &substep_integration_parameters,
                        &mut self.islands,
                        &mut self.broad_phase,
                        &mut self.narrow_phase,
                        &mut self.bodies,
                        &mut self.colliders,
                        &mut self.impulse_joints,
                        &mut self.multibody_joints,
                        &mut self.ccd_solver,
                        None,
                        hooks,
                        events,
                    );
                }
            }
            TimestepMode::Fixed { dt, substeps } => {
                self.integration_parameters.dt = dt;

                let mut substep_integration_parameters = self.integration_parameters;
                substep_integration_parameters.dt = dt / (substeps as Real);

                for _ in 0..substeps {
                    self.pipeline.step(
                        &gravity.into(),
                        &substep_integration_parameters,
                        &mut self.islands,
                        &mut self.broad_phase,
                        &mut self.narrow_phase,
                        &mut self.bodies,
                        &mut self.colliders,
                        &mut self.impulse_joints,
                        &mut self.multibody_joints,
                        &mut self.ccd_solver,
                        None,
                        hooks,
                        events,
                    );
                }
            }
        }
    }

    /// This method makes sure that the rigid-body positions have been propagated to
    /// their attached colliders, without having to perform a srimulation step.
    pub fn propagate_modified_body_positions_to_colliders(&mut self) {
        self.bodies
            .propagate_modified_body_positions_to_colliders(&mut self.colliders);
    }

    /// Updates the state of the query pipeline, based on the collider positions known
    /// from the last timestep or the last call to `self.propagate_modified_body_positions_to_colliders()`.
    pub fn update_query_pipeline(&mut self) {
        self.query_pipeline.update(&self.bodies, &self.colliders);
    }

    /// Attempts to move shape, optionally sliding or climbing obstacles.
    ///
    /// # Parameters
    /// * `movement`: the translational movement to apply.
    /// * `shape`: the shape to move.
    /// * `shape_translation`: the initial position of the shape.
    /// * `shape_rotation`: the rotation of the shape.
    /// * `shape_mass`: the mass of the shape to be considered by the impulse calculation if
    ///                 `MoveShapeOptions::apply_impulse_to_dynamic_bodies` is set to true.
    /// * `options`: configures the behavior of the automatic sliding and climbing.
    /// * `filter`: indicates what collider or rigid-body needs to be ignored by the obstacle detection.
    /// * `events`: callback run on each obstacle hit by the shape on its path.
    #[allow(clippy::too_many_arguments)]
    pub fn move_shape(
        &mut self,
        movement: Vect,
        shape: &Collider,
        shape_translation: Vect,
        shape_rotation: Rot,
        shape_mass: Real,
        options: &MoveShapeOptions,
        filter: QueryFilter,
        events: &mut impl FnMut(CharacterCollision),
    ) -> MoveShapeOutput {
        let mut scaled_shape = shape.clone();
        // TODO: how to set a good number of subdivisions, we don’t have access to the
        //       RapierConfiguration::scaled_shape_subdivision here.
        scaled_shape.set_scale(shape.scale, 20);

        let up = options
            .up
            .try_into()
            .expect("The up vector must be non-zero.");
        let autostep = options.autostep.map(|autostep| CharacterAutostep {
            max_height: autostep.max_height,
            min_width: autostep.min_width,
            include_dynamic_bodies: autostep.include_dynamic_bodies,
        });
        let controller = rapier::control::KinematicCharacterController {
            up,
            offset: options.offset,
            slide: options.slide,
            autostep,
            max_slope_climb_angle: options.max_slope_climb_angle,
            min_slope_slide_angle: options.min_slope_slide_angle,
            snap_to_ground: options.snap_to_ground,
            normal_nudge_factor: options.normal_nudge_factor,
        };

        self.character_collisions_collector.clear();

        // TODO: having to grab all the references to avoid having self in
        //       the closure is ugly.
        let dt = self.integration_parameters.dt;
        let colliders = &self.colliders;
        let bodies = &mut self.bodies;
        let query_pipeline = &self.query_pipeline;
        let collisions = &mut self.character_collisions_collector;
        collisions.clear();

        let result = Self::with_query_filter_elts(
            &self.entity2collider,
            &self.entity2body,
            &self.colliders,
            filter,
            move |filter| {
                let result = controller.move_shape(
                    dt,
                    bodies,
                    colliders,
                    query_pipeline,
                    (&scaled_shape).into(),
                    &(shape_translation, shape_rotation).into(),
                    movement.into(),
                    filter,
                    |c| {
                        if let Some(collision) =
                            CharacterCollision::from_raw_with_set(colliders, &c, true)
                        {
                            events(collision);
                        }
                        collisions.push(c);
                    },
                );

                if options.apply_impulse_to_dynamic_bodies {
                    for collision in &*collisions {
                        controller.solve_character_collision_impulses(
                            dt,
                            bodies,
                            colliders,
                            query_pipeline,
                            (&scaled_shape).into(),
                            shape_mass,
                            collision,
                            filter,
                        )
                    }
                }

                result
            },
        );

        MoveShapeOutput {
            effective_translation: result.translation.into(),
            grounded: result.grounded,
            is_sliding_down_slope: result.is_sliding_down_slope,
        }
    }

    /// Find the closest intersection between a ray and a set of collider.
    ///
    /// # Parameters
    /// * `ray_origin`: the starting point of the ray to cast.
    /// * `ray_dir`: the direction of the ray to cast.
    /// * `max_toi`: the maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the length of the ray to `ray.dir.norm() * max_toi`. Use `Real::MAX` for an unbounded ray.
    /// * `solid`: if this is `true` an impact at time 0.0 (i.e. at the ray origin) is returned if
    ///            it starts inside of a shape. If this `false` then the ray will hit the shape's boundary
    ///            even if its starts inside of it.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn cast_ray(
        &self,
        ray_origin: Vect,
        ray_dir: Vect,
        max_toi: Real,
        solid: bool,
        filter: QueryFilter,
    ) -> Option<(Entity, Real)> {
        let ray = Ray::new(ray_origin.into(), ray_dir.into());

        let (h, toi) = self.with_query_filter(filter, move |filter| {
            self.query_pipeline.cast_ray(
                &self.bodies,
                &self.colliders,
                &ray,
                max_toi,
                solid,
                filter,
            )
        })?;

        self.collider_entity(h).map(|e| (e, toi))
    }

    /// Find the closest intersection between a ray and a set of collider.
    ///
    /// # Parameters
    /// * `ray_origin`: the starting point of the ray to cast.
    /// * `ray_dir`: the direction of the ray to cast.
    /// * `max_toi`: the maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the length of the ray to `ray.dir.norm() * max_toi`. Use `Real::MAX` for an unbounded ray.
    /// * `solid`: if this is `true` an impact at time 0.0 (i.e. at the ray origin) is returned if
    ///            it starts inside of a shape. If this `false` then the ray will hit the shape's boundary
    ///            even if its starts inside of it.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn cast_ray_and_get_normal(
        &self,
        ray_origin: Vect,
        ray_dir: Vect,
        max_toi: Real,
        solid: bool,
        filter: QueryFilter,
    ) -> Option<(Entity, RayIntersection)> {
        let ray = Ray::new(ray_origin.into(), ray_dir.into());

        let (h, result) = self.with_query_filter(filter, move |filter| {
            self.query_pipeline.cast_ray_and_get_normal(
                &self.bodies,
                &self.colliders,
                &ray,
                max_toi,
                solid,
                filter,
            )
        })?;

        self.collider_entity(h)
            .map(|e| (e, RayIntersection::from_rapier(result, ray_origin, ray_dir)))
    }

    /// Find the all intersections between a ray and a set of collider and passes them to a callback.
    ///
    /// # Parameters
    /// * `ray_origin`: the starting point of the ray to cast.
    /// * `ray_dir`: the direction of the ray to cast.
    /// * `max_toi`: the maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the length of the ray to `ray.dir.norm() * max_toi`. Use `Real::MAX` for an unbounded ray.
    /// * `solid`: if this is `true` an impact at time 0.0 (i.e. at the ray origin) is returned if
    ///            it starts inside of a shape. If this `false` then the ray will hit the shape's boundary
    ///            even if its starts inside of it.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    /// * `callback`: function executed on each collider for which a ray intersection has been found.
    ///               There is no guarantees on the order the results will be yielded. If this callback returns `false`,
    ///               this method will exit early, ignore any further raycast.
    #[allow(clippy::too_many_arguments)]
    pub fn intersections_with_ray(
        &self,
        ray_origin: Vect,
        ray_dir: Vect,
        max_toi: Real,
        solid: bool,
        filter: QueryFilter,
        mut callback: impl FnMut(Entity, RayIntersection) -> bool,
    ) {
        let ray = Ray::new(ray_origin.into(), ray_dir.into());
        let callback = |h, inter: rapier::prelude::RayIntersection| {
            self.collider_entity(h)
                .map(|e| callback(e, RayIntersection::from_rapier(inter, ray_origin, ray_dir)))
                .unwrap_or(true)
        };

        self.with_query_filter(filter, move |filter| {
            self.query_pipeline.intersections_with_ray(
                &self.bodies,
                &self.colliders,
                &ray,
                max_toi,
                solid,
                filter,
                callback,
            )
        });
    }

    /// Gets the handle of up to one collider intersecting the given shape.
    ///
    /// # Parameters
    /// * `shape_pos` - The position of the shape used for the intersection test.
    /// * `shape` - The shape used for the intersection test.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn intersection_with_shape(
        &self,
        shape_pos: Vect,
        shape_rot: Rot,
        shape: &Collider,
        filter: QueryFilter,
    ) -> Option<Entity> {
        let scaled_transform = (shape_pos, shape_rot).into();
        let mut scaled_shape = shape.clone();
        // TODO: how to set a good number of subdivisions, we don’t have access to the
        //       RapierConfiguration::scaled_shape_subdivision here.
        scaled_shape.set_scale(shape.scale, 20);

        let h = self.with_query_filter(filter, move |filter| {
            self.query_pipeline.intersection_with_shape(
                &self.bodies,
                &self.colliders,
                &scaled_transform,
                &*scaled_shape.raw,
                filter,
            )
        })?;

        self.collider_entity(h)
    }

    /// Find the projection of a point on the closest collider.
    ///
    /// # Parameters
    /// * `point` - The point to project.
    /// * `solid` - If this is set to `true` then the collider shapes are considered to
    ///   be plain (if the point is located inside of a plain shape, its projection is the point
    ///   itself). If it is set to `false` the collider shapes are considered to be hollow
    ///   (if the point is located inside of an hollow shape, it is projected on the shape's
    ///   boundary).
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn project_point(
        &self,
        point: Vect,
        solid: bool,
        filter: QueryFilter,
    ) -> Option<(Entity, PointProjection)> {
        let (h, result) = self.with_query_filter(filter, move |filter| {
            self.query_pipeline.project_point(
                &self.bodies,
                &self.colliders,
                &point.into(),
                solid,
                filter,
            )
        })?;

        self.collider_entity(h)
            .map(|e| (e, PointProjection::from_rapier(result)))
    }

    /// Find all the colliders containing the given point.
    ///
    /// # Parameters
    /// * `point` - The point used for the containment test.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    /// * `callback` - A function called with each collider with a shape containing the `point`.
    ///                If this callback returns `false`, this method will exit early, ignore any
    ///                further point projection.
    pub fn intersections_with_point(
        &self,
        point: Vect,
        filter: QueryFilter,
        mut callback: impl FnMut(Entity) -> bool,
    ) {
        #[allow(clippy::redundant_closure)]
        // False-positive, we can't move callback, closure becomes `FnOnce`
        let callback = |h| self.collider_entity(h).map(|e| callback(e)).unwrap_or(true);

        self.with_query_filter(filter, move |filter| {
            self.query_pipeline.intersections_with_point(
                &self.bodies,
                &self.colliders,
                &point.into(),
                filter,
                callback,
            )
        });
    }

    /// Find the projection of a point on the closest collider.
    ///
    /// The results include the ID of the feature hit by the point.
    ///
    /// # Parameters
    /// * `point` - The point to project.
    /// * `solid` - If this is set to `true` then the collider shapes are considered to
    ///   be plain (if the point is located inside of a plain shape, its projection is the point
    ///   itself). If it is set to `false` the collider shapes are considered to be hollow
    ///   (if the point is located inside of an hollow shape, it is projected on the shape's
    ///   boundary).
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn project_point_and_get_feature(
        &self,
        point: Vect,
        filter: QueryFilter,
    ) -> Option<(Entity, PointProjection, FeatureId)> {
        let (h, proj, fid) = self.with_query_filter(filter, move |filter| {
            self.query_pipeline.project_point_and_get_feature(
                &self.bodies,
                &self.colliders,
                &point.into(),
                filter,
            )
        })?;

        self.collider_entity(h)
            .map(|e| (e, PointProjection::from_rapier(proj), fid))
    }

    /// Finds all entities of all the colliders with an Aabb intersecting the given Aabb.
    #[cfg(not(feature = "headless"))]
    pub fn colliders_with_aabb_intersecting_aabb(
        &self,
        aabb: bevy::render::primitives::Aabb,
        mut callback: impl FnMut(Entity) -> bool,
    ) {
        #[cfg(feature = "dim2")]
        let scaled_aabb = rapier::prelude::Aabb {
            mins: aabb.min().xy().into(),
            maxs: aabb.max().xy().into(),
        };
        #[cfg(feature = "dim3")]
        let scaled_aabb = rapier::prelude::Aabb {
            mins: aabb.min().into(),
            maxs: aabb.max().into(),
        };
        #[allow(clippy::redundant_closure)]
        // False-positive, we can't move callback, closure becomes `FnOnce`
        let callback = |h: &ColliderHandle| {
            self.collider_entity(*h)
                .map(|e| callback(e))
                .unwrap_or(true)
        };
        self.query_pipeline
            .colliders_with_aabb_intersecting_aabb(&scaled_aabb, callback);
    }

    /// Casts a shape at a constant linear velocity and retrieve the first collider it hits.
    ///
    /// This is similar to ray-casting except that we are casting a whole shape instead of just a
    /// point (the ray origin). In the resulting `ShapeCastHit`, witness and normal 1 refer to the world
    /// collider, and are in world space.
    ///
    /// # Parameters
    /// * `shape_pos` - The initial translation of the shape to cast.
    /// * `shape_rot` - The rotation of the shape to cast.
    /// * `shape_vel` - The constant velocity of the shape to cast (i.e. the cast direction).
    /// * `shape` - The shape to cast.
    /// * `max_toi` - The maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the distance traveled by the shape to `shapeVel.norm() * maxToi`.
    /// * `stop_at_penetration` - If the casted shape starts in a penetration state with any
    ///    collider, two results are possible. If `stop_at_penetration` is `true` then, the
    ///    result will have a `toi` equal to `start_time`. If `stop_at_penetration` is `false`
    ///    then the nonlinear shape-casting will see if further motion wrt. the penetration normal
    ///    would result in tunnelling. If it does not (i.e. we have a separating velocity along
    ///    that normal) then the nonlinear shape-casting will attempt to find another impact,
    ///    at a time `> start_time` that could result in tunnelling.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    #[allow(clippy::too_many_arguments)]
    pub fn cast_shape(
        &self,
        shape_pos: Vect,
        shape_rot: Rot,
        shape_vel: Vect,
        shape: &Collider,
        options: ShapeCastOptions,
        filter: QueryFilter,
    ) -> Option<(Entity, ShapeCastHit)> {
        let scaled_transform = (shape_pos, shape_rot).into();
        let mut scaled_shape = shape.clone();
        // TODO: how to set a good number of subdivisions, we don’t have access to the
        //       RapierConfiguration::scaled_shape_subdivision here.
        scaled_shape.set_scale(shape.scale, 20);

        let (h, result) = self.with_query_filter(filter, move |filter| {
            self.query_pipeline.cast_shape(
                &self.bodies,
                &self.colliders,
                &scaled_transform,
                &shape_vel.into(),
                &*scaled_shape.raw,
                options,
                filter,
            )
        })?;

        self.collider_entity(h).map(|e| {
            (
                e,
                ShapeCastHit::from_rapier(result, options.compute_impact_geometry_on_penetration),
            )
        })
    }

    /* TODO: we need to wrap the NonlinearRigidMotion somehow.
     *
    /// Casts a shape with an arbitrary continuous motion and retrieve the first collider it hits.
    ///
    /// In the resulting `ShapeCastHit`, witness and normal 1 refer to the world collider, and are
    /// in world space.
    ///
    /// # Parameters
    /// * `shape_motion` - The motion of the shape.
    /// * `shape` - The shape to cast.
    /// * `start_time` - The starting time of the interval where the motion takes place.
    /// * `end_time` - The end time of the interval where the motion takes place.
    /// * `stop_at_penetration` - If the casted shape starts in a penetration state with any
    ///    collider, two results are possible. If `stop_at_penetration` is `true` then, the
    ///    result will have a `toi` equal to `start_time`. If `stop_at_penetration` is `false`
    ///    then the nonlinear shape-casting will see if further motion wrt. the penetration normal
    ///    would result in tunnelling. If it does not (i.e. we have a separating velocity along
    ///    that normal) then the nonlinear shape-casting will attempt to find another impact,
    ///    at a time `> start_time` that could result in tunnelling.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn nonlinear_cast_shape(
        &self,
        shape_motion: &NonlinearRigidMotion,
        shape: &Collider,
        start_time: Real,
        end_time: Real,
        stop_at_penetration: bool,
        filter: QueryFilter,
    ) -> Option<(Entity, Toi)> {
        let scaled_transform = (shape_pos, shape_rot).into();
        let mut scaled_shape = shape.clone();
        // TODO: how to set a good number of subdivisions, we don’t have access to the
        //       RapierConfiguration::scaled_shape_subdivision here.
        scaled_shape.set_scale(shape.scale, 20);

        let (h, result) = self.with_query_filter(filter, move |filter| {
            self.query_pipeline.nonlinear_cast_shape(
                &self.bodies,
                &self.colliders,
                shape_motion,
                &*scaled_shape.raw,
                start_time,
                end_time,
                stop_at_penetration,
                filter,
            )
        })?;

        self.collider_entity(h).map(|e| (e, result))
    }
     */

    /// Retrieve all the colliders intersecting the given shape.
    ///
    /// # Parameters
    /// * `shapePos` - The position of the shape to test.
    /// * `shapeRot` - The orientation of the shape to test.
    /// * `shape` - The shape to test.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    /// * `callback` - A function called with the entities of each collider intersecting the `shape`.
    pub fn intersections_with_shape(
        &self,
        shape_pos: Vect,
        shape_rot: Rot,
        shape: &Collider,
        filter: QueryFilter,
        mut callback: impl FnMut(Entity) -> bool,
    ) {
        let scaled_transform = (shape_pos, shape_rot).into();
        let mut scaled_shape = shape.clone();
        // TODO: how to set a good number of subdivisions, we don’t have access to the
        //       RapierConfiguration::scaled_shape_subdivision here.
        scaled_shape.set_scale(shape.scale, 20);

        #[allow(clippy::redundant_closure)]
        // False-positive, we can't move callback, closure becomes `FnOnce`
        let callback = |h| self.collider_entity(h).map(|e| callback(e)).unwrap_or(true);

        self.with_query_filter(filter, move |filter| {
            self.query_pipeline.intersections_with_shape(
                &self.bodies,
                &self.colliders,
                &scaled_transform,
                &*scaled_shape.raw,
                filter,
                callback,
            )
        });
    }
}

#[derive(Debug)]
pub enum WorldError {
    WorldNotFound { world_id: WorldId },
}

impl fmt::Display for WorldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::WorldNotFound { world_id } => write!(f, "World with id {world_id} not found."),
        }
    }
}

impl std::error::Error for WorldError {}

/// The Rapier context, containing all the state of the physics engine.
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
#[derive(Resource)]
pub struct RapierContext {
    /// Stores all the worlds in the simulation.
    pub worlds: HashMap<WorldId, RapierWorld>,

    next_world_id: WorldId,
}

impl RapierContext {}

impl Default for RapierContext {
    fn default() -> Self {
        Self::new(RapierWorld::default())
    }
}

impl RapierContext {
    /// Creates a new RapierContext with a custom starting world
    pub fn new(world: RapierWorld) -> Self {
        let mut worlds = HashMap::new();
        worlds.insert(DEFAULT_WORLD_ID, world);

        Self {
            worlds,
            next_world_id: WorldId::new(1),
        }
    }

    /// Adds a world to the simulation
    ///
    /// Returns that world's id
    pub fn add_world(&mut self, world: RapierWorld) -> WorldId {
        let world_id = self.next_world_id;

        self.worlds.insert(world_id, world);

        self.next_world_id.0 += 1;

        world_id
    }

    /// Removes a world from the simulation. This does NOT despawn entities within that world.
    /// Make sure all entities within that world are despawned or moved to a seperate world.
    ///
    /// Returns the removed world or an err if that world wasn't found or you tried to remove the default world.
    pub fn remove_world(&mut self, world_id: WorldId) -> Result<RapierWorld, WorldError> {
        self.worlds
            .remove(&world_id)
            .ok_or(WorldError::WorldNotFound { world_id })
    }

    /// Gets the world at the given id. If the world does not exist, an Err result will be returned
    pub fn get_world(&self, world_id: WorldId) -> Result<&RapierWorld, WorldError> {
        self.worlds
            .get(&world_id)
            .ok_or(WorldError::WorldNotFound { world_id })
    }

    /// Gets the mutable world at the given id. If the world does not exist, an Err result will be returned
    pub fn get_world_mut(&mut self, world_id: WorldId) -> Result<&mut RapierWorld, WorldError> {
        self.worlds
            .get_mut(&world_id)
            .ok_or(WorldError::WorldNotFound { world_id })
    }

    fn get_collider_parent_from_world(
        &self,
        entity: Entity,
        world: &RapierWorld,
    ) -> Option<Entity> {
        world
            .entity2collider
            .get(&entity)
            .and_then(|h| world.colliders.get(*h))
            .and_then(|co| co.parent())
            .and_then(|h| self.rigid_body_entity(h))
    }

    /// If the collider attached to `entity` is attached to a rigid-body, this
    /// returns the `Entity` containing that rigid-body.
    pub fn collider_parent(&self, entity: Entity) -> Option<Entity> {
        for (_, world) in self.worlds.iter() {
            if let Some(entity) = self.get_collider_parent_from_world(entity, world) {
                return Some(entity);
            }
        }

        None
    }

    /// If the collider attached to `entity` is attached to a rigid-body, this
    /// returns the `Entity` containing that rigid-body.
    ///
    /// If the world does not exist, this returns None
    pub fn collider_parent_for_world(&self, entity: Entity, world_id: WorldId) -> Option<Entity> {
        if let Some(world) = self.worlds.get(&world_id) {
            self.get_collider_parent_from_world(entity, world)
        } else {
            None
        }
    }

    /// Retrieve the Bevy entity the given Rapier collider (identified by its handle) is attached.
    pub fn collider_entity(&self, handle: ColliderHandle) -> Option<Entity> {
        for (_, world) in self.worlds.iter() {
            let entity = RapierWorld::collider_entity_with_set(&world.colliders, handle);
            if entity.is_some() {
                return entity;
            }
        }

        None
    }

    /// Retrieve the Bevy entity the given Rapier rigid-body (identified by its handle) is attached.
    pub fn rigid_body_entity(&self, handle: RigidBodyHandle) -> Option<Entity> {
        for (_, world) in self.worlds.iter() {
            let entity = world.rigid_body_entity(handle);
            if entity.is_some() {
                return entity;
            }
        }

        None
    }

    /// Retrieve the Bevy entity the given Rapier rigid-body (identified by its handle) is attached.
    ///
    /// Returns None if this world does not exist
    pub fn rigid_body_entity_in_world(
        &self,
        handle: RigidBodyHandle,
        world_id: WorldId,
    ) -> Option<Entity> {
        self.worlds
            .get(&world_id)
            .map(|world| world.rigid_body_entity(handle))
            .unwrap_or(None)
    }

    /// Advance the simulation, based on the given timestep mode.
    #[allow(clippy::too_many_arguments)]
    pub fn step_simulation(
        mut self,
        timestep_mode: TimestepMode,
        mut events: Option<(EventWriter<CollisionEvent>, EventWriter<ContactForceEvent>)>,
        hooks: &dyn PhysicsHooks,
        time: &Time,
        sim_to_render_time: &mut SimulationToRenderTime,
        mut interpolation_query: Option<
            &mut Query<(&RapierRigidBodyHandle, &mut TransformInterpolation)>,
        >,
    ) {
        for (world_id, world) in self.worlds.iter_mut() {
            world.step_simulation(
                *world_id,
                timestep_mode,
                events.is_some(),
                hooks,
                time,
                sim_to_render_time,
                &mut interpolation_query,
            );

            if let Some((collision_event_writer, contact_force_event_writer)) = &mut events {
                world.send_bevy_events(collision_event_writer, contact_force_event_writer);
            }
        }
    }

    /// This method makes sure that the rigid-body positions have been propagated to
    /// their attached colliders, without having to perform a srimulation step.
    pub fn propagate_modified_body_positions_to_colliders(&mut self) {
        for (_, world) in self.worlds.iter_mut() {
            world.propagate_modified_body_positions_to_colliders();
        }
    }

    /// This method makes sure that the rigid-body positions have been propagated to
    /// their attached colliders, without having to perform a srimulation step.
    ///
    /// Returns Ok if the world was found, Err(WorldError::WorldNotFound) if the world was not found.
    pub fn propagate_modified_body_positions_to_colliders_for_world(
        &mut self,
        world_id: WorldId,
    ) -> Result<(), WorldError> {
        match self.worlds.get_mut(&world_id) {
            Some(world) => {
                world.propagate_modified_body_positions_to_colliders();

                Ok(())
            }
            None => Err(WorldError::WorldNotFound { world_id }),
        }
    }

    /// Updates the state of the query pipeline, based on the collider positions known
    /// from the last timestep or the last call to `self.propagate_modified_body_positions_to_colliders()`.
    pub fn update_query_pipeline(&mut self) {
        for (_, world) in self.worlds.iter_mut() {
            world.update_query_pipeline();
        }
    }

    /// The map from entities to rigid-body handles.
    ///
    /// Returns Err if the world doesn't exist, or the entity2body if it does
    pub fn entity2body(
        &self,
        world_id: WorldId,
    ) -> Result<&HashMap<Entity, RigidBodyHandle>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |x| {
                Ok(&x.entity2body)
            })
    }

    /// The map from entities to collider handles.
    pub fn entity2collider(
        &self,
        world_id: WorldId,
    ) -> Result<&HashMap<Entity, ColliderHandle>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |x| {
                Ok(&x.entity2collider)
            })
    }

    /// The map from entities to impulse joint handles.
    pub fn entity2impulse_joint(
        &self,
        world_id: WorldId,
    ) -> Result<&HashMap<Entity, ImpulseJointHandle>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |x| {
                Ok(&x.entity2impulse_joint)
            })
    }

    /// The map from entities to multibody joint handles.
    pub fn entity2multibody_joint(
        &self,
        world_id: WorldId,
    ) -> Result<&HashMap<Entity, MultibodyJointHandle>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |x| {
                Ok(&x.entity2multibody_joint)
            })
    }

    /// Attempts to move shape, optionally sliding or climbing obstacles.
    ///
    /// # Parameters
    /// * `movement`: the translational movement to apply.
    /// * `shape`: the shape to move.
    /// * `shape_translation`: the initial position of the shape.
    /// * `shape_rotation`: the rotation of the shape.
    /// * `shape_mass`: the mass of the shape to be considered by the impulse calculation if
    ///                 `MoveShapeOptions::apply_impulse_to_dynamic_bodies` is set to true.
    /// * `options`: configures the behavior of the automatic sliding and climbing.
    /// * `filter`: indicates what collider or rigid-body needs to be ignored by the obstacle detection.
    /// * `events`: callback run on each obstacle hit by the shape on its path.
    #[allow(clippy::too_many_arguments)]
    pub fn move_shape(
        &mut self,
        world_id: WorldId,
        movement: Vect,
        shape: &Collider,
        shape_translation: Vect,
        shape_rotation: Rot,
        shape_mass: Real,
        options: &MoveShapeOptions,
        filter: QueryFilter,
        events: &mut impl FnMut(CharacterCollision),
    ) -> Result<MoveShapeOutput, WorldError> {
        self.worlds.get_mut(&world_id).map_or(
            Err(WorldError::WorldNotFound { world_id }),
            |world| {
                Ok(world.move_shape(
                    movement,
                    shape,
                    shape_translation,
                    shape_rotation,
                    shape_mass,
                    options,
                    filter,
                    events,
                ))
            },
        )
    }

    /// Find the closest intersection between a ray and a set of collider.
    ///
    /// # Parameters
    /// * `world_id`: the world to cast this ray in. Use DEFAULT_WORLD_ID for a single-world simulation
    /// * `ray_origin`: the starting point of the ray to cast.
    /// * `ray_dir`: the direction of the ray to cast.
    /// * `max_toi`: the maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the length of the ray to `ray.dir.norm() * max_toi`. Use `Real::MAX` for an unbounded ray.
    /// * `solid`: if this is `true` an impact at time 0.0 (i.e. at the ray origin) is returned if
    ///            it starts inside of a shape. If this `false` then the ray will hit the shape's boundary
    ///            even if its starts inside of it.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn cast_ray(
        &self,
        world_id: WorldId,
        ray_origin: Vect,
        ray_dir: Vect,
        max_toi: Real,
        solid: bool,
        filter: QueryFilter,
    ) -> Result<Option<(Entity, Real)>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                Ok(world.cast_ray(ray_origin, ray_dir, max_toi, solid, filter))
            })
    }

    /// Find the closest intersection between a ray and a set of collider.
    ///
    /// # Parameters
    /// * `world_id`: the world to cast this ray in. Use DEFAULT_WORLD_ID for a single-world simulation
    /// * `ray_origin`: the starting point of the ray to cast.
    /// * `ray_dir`: the direction of the ray to cast.
    /// * `max_toi`: the maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the length of the ray to `ray.dir.norm() * max_toi`. Use `Real::MAX` for an unbounded ray.
    /// * `solid`: if this is `true` an impact at time 0.0 (i.e. at the ray origin) is returned if
    ///            it starts inside of a shape. If this `false` then the ray will hit the shape's boundary
    ///            even if its starts inside of it.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn cast_ray_and_get_normal(
        &self,
        world_id: WorldId,
        ray_origin: Vect,
        ray_dir: Vect,
        max_toi: Real,
        solid: bool,
        filter: QueryFilter,
    ) -> Result<Option<(Entity, RayIntersection)>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                Ok(world.cast_ray_and_get_normal(ray_origin, ray_dir, max_toi, solid, filter))
            })
    }

    /// Find the all intersections between a ray and a set of collider and passes them to a callback.
    ///
    /// # Parameters
    /// * `world_id`: the world to cast this ray in. Use DEFAULT_WORLD_ID for a single-world simulation
    /// * `ray_origin`: the starting point of the ray to cast.
    /// * `ray_dir`: the direction of the ray to cast.
    /// * `max_toi`: the maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the length of the ray to `ray.dir.norm() * max_toi`. Use `Real::MAX` for an unbounded ray.
    /// * `solid`: if this is `true` an impact at time 0.0 (i.e. at the ray origin) is returned if
    ///            it starts inside of a shape. If this `false` then the ray will hit the shape's boundary
    ///            even if its starts inside of it.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    /// * `callback`: function executed on each collider for which a ray intersection has been found.
    ///               There is no guarantees on the order the results will be yielded. If this callback returns `false`,
    ///               this method will exit early, ignore any further raycast.
    #[allow(clippy::too_many_arguments)]
    pub fn intersections_with_ray(
        &self,
        world_id: WorldId,
        ray_origin: Vect,
        ray_dir: Vect,
        max_toi: Real,
        solid: bool,
        filter: QueryFilter,
        callback: impl FnMut(Entity, RayIntersection) -> bool,
    ) -> Result<(), WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                world.intersections_with_ray(ray_origin, ray_dir, max_toi, solid, filter, callback);
                Ok(())
            })
    }

    /// Gets the handle of up to one collider intersecting the given shape.
    ///
    /// # Parameters
    /// * `shape_pos` - The position of the shape used for the intersection test.
    /// * `shape` - The shape used for the intersection test.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn intersection_with_shape(
        &self,
        world_id: WorldId,
        shape_pos: Vect,
        shape_rot: Rot,
        shape: &Collider,
        filter: QueryFilter,
    ) -> Result<Option<Entity>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                Ok(world.intersection_with_shape(shape_pos, shape_rot, shape, filter))
            })
    }

    /// Find the projection of a point on the closest collider.
    ///
    /// # Parameters
    /// * `point` - The point to project.
    /// * `solid` - If this is set to `true` then the collider shapes are considered to
    ///   be plain (if the point is located inside of a plain shape, its projection is the point
    ///   itself). If it is set to `false` the collider shapes are considered to be hollow
    ///   (if the point is located inside of an hollow shape, it is projected on the shape's
    ///   boundary).
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn project_point(
        &self,
        world_id: WorldId,
        point: Vect,
        solid: bool,
        filter: QueryFilter,
    ) -> Result<Option<(Entity, PointProjection)>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                Ok(world.project_point(point, solid, filter))
            })
    }

    /// Find all the colliders containing the given point.
    ///
    /// # Parameters
    /// * `point` - The point used for the containment test.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    /// * `callback` - A function called with each collider with a shape containing the `point`.
    ///                If this callback returns `false`, this method will exit early, ignore any
    ///                further point projection.
    pub fn intersections_with_point(
        &self,
        world_id: WorldId,
        point: Vect,
        filter: QueryFilter,
        callback: impl FnMut(Entity) -> bool,
    ) -> Result<(), WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                world.intersections_with_point(point, filter, callback);
                Ok(())
            })
    }

    /// Find the projection of a point on the closest collider.
    ///
    /// The results include the ID of the feature hit by the point.
    ///
    /// # Parameters
    /// * `point` - The point to project.
    /// * `solid` - If this is set to `true` then the collider shapes are considered to
    ///   be plain (if the point is located inside of a plain shape, its projection is the point
    ///   itself). If it is set to `false` the collider shapes are considered to be hollow
    ///   (if the point is located inside of an hollow shape, it is projected on the shape's
    ///   boundary).
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn project_point_and_get_feature(
        &self,
        world_id: WorldId,
        point: Vect,
        filter: QueryFilter,
    ) -> Result<Option<(Entity, PointProjection, FeatureId)>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                Ok(world.project_point_and_get_feature(point, filter))
            })
    }

    /// Finds all entities of all the colliders with an Aabb intersecting the given Aabb.
    pub fn colliders_with_aabb_intersecting_aabb(
        &self,
        world_id: WorldId,
        aabb: bevy::render::primitives::Aabb,
        callback: impl FnMut(Entity) -> bool,
    ) -> Result<(), WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                world.colliders_with_aabb_intersecting_aabb(aabb, callback);
                Ok(())
            })
    }

    /// Casts a shape at a constant linear velocity and retrieve the first collider it hits.
    ///
    /// This is similar to ray-casting except that we are casting a whole shape instead of just a
    /// point (the ray origin). In the resulting `TOI`, witness and normal 1 refer to the world
    /// collider, and are in world space.
    ///
    /// # Parameters
    /// * `shape_pos` - The initial position of the shape to cast.
    /// * `shape_vel` - The constant velocity of the shape to cast (i.e. the cast direction).
    /// * `shape` - The shape to cast.
    /// * `max_toi` - The maximum time-of-impact that can be reported by this cast. This effectively
    ///   limits the distance traveled by the shape to `shapeVel.norm() * maxToi`.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    #[allow(clippy::too_many_arguments)]
    pub fn cast_shape(
        &self,
        world_id: WorldId,
        shape_pos: Vect,
        shape_rot: Rot,
        shape_vel: Vect,
        shape: &Collider,
        options: ShapeCastOptions,
        filter: QueryFilter,
    ) -> Result<Option<(Entity, ShapeCastHit)>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                Ok(world.cast_shape(shape_pos, shape_rot, shape_vel, shape, options, filter))
            })
    }

    /* TODO: we need to wrap the NonlinearRigidMotion somehow.
     *
    /// Casts a shape with an arbitrary continuous motion and retrieve the first collider it hits.
    ///
    /// In the resulting `TOI`, witness and normal 1 refer to the world collider, and are in world
    /// space.
    ///
    /// # Parameters
    /// * `shape_motion` - The motion of the shape.
    /// * `shape` - The shape to cast.
    /// * `start_time` - The starting time of the interval where the motion takes place.
    /// * `end_time` - The end time of the interval where the motion takes place.
    /// * `stop_at_penetration` - If the casted shape starts in a penetration state with any
    ///    collider, two results are possible. If `stop_at_penetration` is `true` then, the
    ///    result will have a `toi` equal to `start_time`. If `stop_at_penetration` is `false`
    ///    then the nonlinear shape-casting will see if further motion wrt. the penetration normal
    ///    would result in tunnelling. If it does not (i.e. we have a separating velocity along
    ///    that normal) then the nonlinear shape-casting will attempt to find another impact,
    ///    at a time `> start_time` that could result in tunnelling.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    pub fn nonlinear_cast_shape(
        &self,
        world_id: WorldId,
        shape_motion: &NonlinearRigidMotion,
        shape: &Collider,
        start_time: Real,
        end_time: Real,
        stop_at_penetration: bool,
        filter: QueryFilter,
    ) -> Result<Option<(Entity, Toi)>, WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                Ok(world.nonlinear_cast_shape(shape_motion, shape, start_time, end_time, stop_at_penetration, filter))
            })
    }
     */

    /// Retrieve all the colliders intersecting the given shape.
    ///
    /// # Parameters
    /// * `shapePos` - The position of the shape to test.
    /// * `shapeRot` - The orientation of the shape to test.
    /// * `shape` - The shape to test.
    /// * `filter`: set of rules used to determine which collider is taken into account by this scene query.
    /// * `callback` - A function called with the entities of each collider intersecting the `shape`.
    pub fn intersections_with_shape(
        &self,
        world_id: WorldId,
        shape_pos: Vect,
        shape_rot: Rot,
        shape: &Collider,
        filter: QueryFilter,
        callback: impl FnMut(Entity) -> bool,
    ) -> Result<(), WorldError> {
        self.worlds
            .get(&world_id)
            .map_or(Err(WorldError::WorldNotFound { world_id }), |world| {
                world.intersections_with_shape(shape_pos, shape_rot, shape, filter, callback);
                Ok(())
            })
    }
}
