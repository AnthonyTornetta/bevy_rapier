use crate::dynamics::RapierRigidBodyHandle;
use crate::plugin::context::systemparams::RAPIER_CONTEXT_EXPECT_ERROR;
use crate::plugin::context::{
    DefaultRapierContext, RapierContextColliders, RapierContextEntityLink, RapierRigidBodySet,
};
use crate::plugin::{configuration::TimestepMode, RapierConfiguration};
use crate::{dynamics::RigidBody, plugin::context::SimulationToRenderTime};
use crate::{prelude::*, utils};
use bevy::prelude::*;
use rapier::dynamics::{RigidBodyBuilder, RigidBodyHandle, RigidBodyType};
use std::collections::HashMap;

/// Components that will be updated after a physics step.
pub type RigidBodyWritebackComponents<'a> = (
    &'a RapierRigidBodyHandle,
    &'a RapierContextEntityLink,
    Option<&'a mut Transform>,
    Option<&'a RigidBody>,
    Option<&'a mut TransformInterpolation>,
    Option<&'a mut Velocity>,
    Option<&'a mut Sleeping>,
);

/// Components related to rigid-bodies.
pub type RigidBodyComponents<'a> = (
    (Entity, Option<&'a RapierContextEntityLink>),
    &'a RigidBody,
    Option<&'a GlobalTransform>,
    Option<&'a Velocity>,
    Option<&'a AdditionalMassProperties>,
    Option<&'a ReadMassProperties>,
    Option<&'a LockedAxes>,
    Option<&'a ExternalForce>,
    Option<&'a GravityScale>,
    (Option<&'a Ccd>, Option<&'a SoftCcd>),
    Option<&'a Dominance>,
    Option<&'a Sleeping>,
    Option<&'a Damping>,
    Option<&'a RigidBodyDisabled>,
    Option<&'a AdditionalSolverIterations>,
);

/// System responsible for applying changes the user made to a rigid-body-related component.
pub fn apply_rigid_body_user_changes(
    mut rigid_body_sets: Query<&mut RapierRigidBodySet>,
    config: Query<&RapierConfiguration>,
    changed_rb_types: Query<
        (&RapierRigidBodyHandle, &RapierContextEntityLink, &RigidBody),
        Changed<RigidBody>,
    >,
    mut changed_transforms: Query<
        (
            &RapierRigidBodyHandle,
            &RapierContextEntityLink,
            &GlobalTransform,
            Option<&mut TransformInterpolation>,
        ),
        Changed<GlobalTransform>,
    >,
    changed_velocities: Query<
        (&RapierRigidBodyHandle, &RapierContextEntityLink, &Velocity),
        Changed<Velocity>,
    >,
    changed_additional_mass_props: Query<
        (
            Entity,
            &RapierContextEntityLink,
            &RapierRigidBodyHandle,
            &AdditionalMassProperties,
        ),
        Changed<AdditionalMassProperties>,
    >,
    changed_locked_axes: Query<
        (
            &RapierRigidBodyHandle,
            &RapierContextEntityLink,
            &LockedAxes,
        ),
        Changed<LockedAxes>,
    >,
    changed_forces: Query<
        (
            &RapierRigidBodyHandle,
            &RapierContextEntityLink,
            &ExternalForce,
        ),
        Changed<ExternalForce>,
    >,
    mut changed_impulses: Query<
        (
            &RapierRigidBodyHandle,
            &RapierContextEntityLink,
            &mut ExternalImpulse,
        ),
        Changed<ExternalImpulse>,
    >,
    changed_gravity_scale: Query<
        (
            &RapierRigidBodyHandle,
            &RapierContextEntityLink,
            &GravityScale,
        ),
        Changed<GravityScale>,
    >,
    (changed_ccd, changed_soft_ccd): (
        Query<(&RapierRigidBodyHandle, &RapierContextEntityLink, &Ccd), Changed<Ccd>>,
        Query<(&RapierRigidBodyHandle, &RapierContextEntityLink, &SoftCcd), Changed<SoftCcd>>,
    ),
    changed_dominance: Query<
        (&RapierRigidBodyHandle, &RapierContextEntityLink, &Dominance),
        Changed<Dominance>,
    >,
    changed_sleeping: Query<
        (&RapierRigidBodyHandle, &RapierContextEntityLink, &Sleeping),
        Changed<Sleeping>,
    >,
    changed_damping: Query<
        (&RapierRigidBodyHandle, &RapierContextEntityLink, &Damping),
        Changed<Damping>,
    >,
    (changed_disabled, changed_additional_solver_iterations): (
        Query<
            (
                &RapierRigidBodyHandle,
                &RapierContextEntityLink,
                &RigidBodyDisabled,
            ),
            Changed<RigidBodyDisabled>,
        >,
        Query<
            (
                &RapierRigidBodyHandle,
                &RapierContextEntityLink,
                &AdditionalSolverIterations,
            ),
            Changed<AdditionalSolverIterations>,
        >,
    ),
    mut mass_modified: EventWriter<MassModifiedEvent>,
) {
    // Deal with sleeping first, because other changes may then wake-up the
    // rigid-body again.
    for (handle, link, sleeping) in changed_sleeping.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();

        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            let activation = rb.activation_mut();
            activation.normalized_linear_threshold = sleeping.normalized_linear_threshold;
            activation.angular_threshold = sleeping.angular_threshold;

            if !sleeping.sleeping && activation.sleeping {
                rb.wake_up(true);
            } else if sleeping.sleeping && !activation.sleeping {
                rb.sleep();
            }
        }
    }

    // NOTE: we must change the rigid-body type before updating the
    //       transform or velocity. Otherwise, if the rigid-body was fixed
    //       and changed to anything else, the velocity change wouldn’t have any effect.
    //       Similarly, if the rigid-body was kinematic position-based before and
    //       changed to anything else, a transform change would modify the next
    //       position instead of the current one.
    for (handle, link, rb_type) in changed_rb_types.iter() {
        let context = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = context.bodies.get_mut(handle.0) {
            rb.set_body_type((*rb_type).into(), true);
        }
    }

    // Manually checks if the transform changed.
    // This is needed for detecting if the user actually changed the rigid-body
    // transform, or if it was just the change we made in our `writeback_rigid_bodies`
    // system.
    let transform_changed_fn =
        |handle: &RigidBodyHandle,
         config: &RapierConfiguration,
         transform: &GlobalTransform,
         last_transform_set: &HashMap<RigidBodyHandle, GlobalTransform>| {
            if config.force_update_from_transform_changes {
                true
            } else if let Some(prev) = last_transform_set.get(handle) {
                *prev != *transform
            } else {
                true
            }
        };

    for (handle, link, global_transform, mut interpolation) in changed_transforms.iter_mut() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        let config = config
            .get(link.0)
            .expect("Could not get `RapierConfiguration`");
        // Use an Option<bool> to avoid running the check twice.
        let mut transform_changed = None;

        if let Some(interpolation) = interpolation.as_deref_mut() {
            transform_changed = transform_changed.or_else(|| {
                Some(transform_changed_fn(
                    &handle.0,
                    config,
                    global_transform,
                    &rigidbody_set.last_body_transform_set,
                ))
            });

            if transform_changed == Some(true) {
                // Reset the interpolation so we don’t overwrite
                // the user’s input.
                interpolation.start = None;
                interpolation.end = None;
            }
        }
        // TODO: avoid to run multiple times the mutable deref ?
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            transform_changed = transform_changed.or_else(|| {
                Some(transform_changed_fn(
                    &handle.0,
                    config,
                    global_transform,
                    &rigidbody_set.last_body_transform_set,
                ))
            });

            match rb.body_type() {
                RigidBodyType::KinematicPositionBased => {
                    if transform_changed == Some(true) {
                        rb.set_next_kinematic_position(utils::transform_to_iso(
                            &global_transform.compute_transform(),
                        ));
                        rigidbody_set
                            .last_body_transform_set
                            .insert(handle.0, *global_transform);
                    }
                }
                _ => {
                    if transform_changed == Some(true) {
                        rb.set_position(
                            utils::transform_to_iso(&global_transform.compute_transform()),
                            true,
                        );
                        rigidbody_set
                            .last_body_transform_set
                            .insert(handle.0, *global_transform);
                    }
                }
            }
        }
    }

    for (handle, link, velocity) in changed_velocities.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.set_linvel(velocity.linvel.into(), true);
            #[allow(clippy::useless_conversion)] // Need to convert if dim3 enabled
            rb.set_angvel(velocity.angvel.into(), true);
        }
    }

    for (entity, link, handle, mprops) in changed_additional_mass_props.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            match mprops {
                AdditionalMassProperties::MassProperties(mprops) => {
                    rb.set_additional_mass_properties(mprops.into_rapier(), true);
                }
                AdditionalMassProperties::Mass(mass) => {
                    rb.set_additional_mass(*mass, true);
                }
            }

            mass_modified.write(entity.into());
        }
    }

    for (handle, link, additional_solver_iters) in changed_additional_solver_iterations.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.set_additional_solver_iterations(additional_solver_iters.0);
        }
    }

    for (handle, link, locked_axes) in changed_locked_axes.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.set_locked_axes((*locked_axes).into(), true);
        }
    }

    for (handle, link, forces) in changed_forces.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.reset_forces(true);
            rb.reset_torques(true);
            rb.add_force(forces.force.into(), true);
            #[allow(clippy::useless_conversion)] // Need to convert if dim3 enabled
            rb.add_torque(forces.torque.into(), true);
        }
    }

    for (handle, link, mut impulses) in changed_impulses.iter_mut() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.apply_impulse(impulses.impulse.into(), true);
            #[allow(clippy::useless_conversion)] // Need to convert if dim3 enabled
            rb.apply_torque_impulse(impulses.torque_impulse.into(), true);
            impulses.reset();
        }
    }

    for (handle, link, gravity_scale) in changed_gravity_scale.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.set_gravity_scale(gravity_scale.0, true);
        }
    }

    for (handle, link, ccd) in changed_ccd.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.enable_ccd(ccd.enabled);
        }
    }

    for (handle, link, soft_ccd) in changed_soft_ccd.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.set_soft_ccd_prediction(soft_ccd.prediction);
        }
    }

    for (handle, link, dominance) in changed_dominance.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.set_dominance_group(dominance.groups);
        }
    }

    for (handle, link, damping) in changed_damping.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(rb) = rigidbody_set.bodies.get_mut(handle.0) {
            rb.set_linear_damping(damping.linear_damping);
            rb.set_angular_damping(damping.angular_damping);
        }
    }

    for (handle, link, _) in changed_disabled.iter() {
        let rigidbody_set = rigid_body_sets
            .get_mut(link.0)
            .expect(RAPIER_CONTEXT_EXPECT_ERROR)
            .into_inner();
        if let Some(co) = rigidbody_set.bodies.get_mut(handle.0) {
            co.set_enabled(false);
        }
    }
}

/// System responsible for writing the result of the last simulation step into our `bevy_rapier`
/// components and the [`GlobalTransform`] component.
pub fn writeback_rigid_bodies(
    mut context_access: WriteRapierContext,
    config: Query<&RapierConfiguration>,
    sim_to_render_time: Query<&SimulationToRenderTime>,
    top_entities: Query<Entity, Without<ChildOf>>,
    timestep_mode: Res<TimestepMode>,
    mut writeback: Query<RigidBodyWritebackComponents, Without<RigidBodyDisabled>>,
    q_disabled_trans: Query<&Transform, With<RigidBodyDisabled>>,
    children_query: Query<&Children>,
) {
    for entity in top_entities.iter() {
        let (transform, delta_transform, velocity, world_offset, my_vel_delta) = if let Ok((
            handle,
            link,
            transform,
            _,
            mut interpolation,
            mut velocity,
            mut sleeping,
        )) =
            writeback.get_mut(entity)
        {
            let config = config
                .get(link.0)
                .expect("Could not get `RapierConfiguration`");
            if !config.physics_pipeline_active {
                continue;
            }

            let mut my_new_global_transform = Transform::IDENTITY;
            let mut parent_delta = Transform::IDENTITY;
            let mut my_velocity = Velocity::default();
            let mut world_offset = Vec3::ZERO;
            let mut my_vel_delta = Velocity::default();

            let handle = handle.0;

            let context = context_access
                .rapier_context
                .get_mut(link.0)
                .expect("Invalid link")
                .4
                .into_inner();

            let sim_to_render_time = sim_to_render_time
                .get(link.0)
                .expect("Could not get `SimulationToRenderTime`");

            // TODO: do this the other way round: iterate through Rapier’s RigidBodySet on the active bodies,
            // and update the components accordingly. That way, we don’t have to iterate through the entities that weren’t changed
            // by physics (for example because they are sleeping).
            if let Some(rb) = context.bodies.get(handle) {
                let mut interpolated_pos = utils::iso_to_transform(rb.position());

                if let TimestepMode::Interpolated { dt, .. } = *timestep_mode {
                    if let Some(interpolation) = interpolation.as_deref_mut() {
                        if interpolation.end.is_none() {
                            interpolation.end = Some(*rb.position());
                        }

                        if let Some(interpolated) =
                            interpolation.lerp_slerp((dt + sim_to_render_time.diff) / dt)
                        {
                            interpolated_pos = utils::iso_to_transform(&interpolated);
                        }
                    }
                }

                if let Some(mut transform) = transform {
                    // NOTE: Rapier's `RigidBody` doesn't know its own scale as it is encoded
                    //       directly within its collider, so we have to retrieve it from
                    //       the scale of its bevy transform.
                    interpolated_pos = interpolated_pos.with_scale(transform.scale);

                    world_offset = transform.translation;

                    // let (cur_inv_scale, cur_inv_rotation, cur_inv_translation) = transform
                    //     .compute_affine()
                    //     .inverse()
                    //     .to_scale_rotation_translation();

                    parent_delta = Transform {
                        translation: interpolated_pos.translation - transform.translation,
                        rotation: interpolated_pos.rotation * transform.rotation.inverse(),
                        scale: transform.scale,
                    };

                    let com = rb.center_of_mass();

                    #[cfg(feature = "dim3")]
                    let com = Vec3::new(
                        com.x - rb.translation().x,
                        com.y - rb.translation().y,
                        com.z - rb.translation().z,
                    );
                    #[cfg(feature = "dim2")]
                    let com =
                        Vec3::new(com.x - rb.translation().x, com.y - rb.translation().y, 0.0);

                    let com_diff = com - parent_delta.rotation.mul_vec3(com);
                    parent_delta.translation -= com_diff;

                    #[allow(unused_mut)] // mut is needed in 2D but not in 3D.
                    let mut new_translation = interpolated_pos.translation;

                    // In 2D, preserve the transform `z` component that may have been set by the user
                    #[cfg(feature = "dim2")]
                    {
                        new_translation.z = transform.translation.z;
                    }

                    if transform.rotation != interpolated_pos.rotation
                        || transform.translation != new_translation
                    {
                        // NOTE: we write the new value only if there was an
                        //       actual change, in order to not trigger bevy’s
                        //       change tracking when the values didn’t change.
                        transform.rotation = interpolated_pos.rotation;
                        transform.translation = new_translation;
                    }

                    my_new_global_transform = interpolated_pos;

                    context.last_body_transform_set.insert(
                        handle,
                        GlobalTransform::from(
                            Transform::from_translation(new_translation)
                                .with_rotation(interpolated_pos.rotation),
                        ),
                    );
                }

                if let Some(velocity) = &mut velocity {
                    my_velocity = **velocity;

                    let new_vel = Velocity {
                        linvel: (*rb.linvel()).into(),
                        #[cfg(feature = "dim3")]
                        angvel: (*rb.angvel()).into(),
                        #[cfg(feature = "dim2")]
                        angvel: rb.angvel(),
                    };

                    my_vel_delta = Velocity {
                        linvel: new_vel.linvel - velocity.linvel,
                        ..Default::default()
                    };

                    // NOTE: we write the new value only if there was an
                    //       actual change, in order to not trigger bevy’s
                    //       change tracking when the values didn’t change.
                    if **velocity != new_vel {
                        **velocity = new_vel;
                    }
                }

                if let Some(sleeping) = &mut sleeping {
                    // NOTE: we write the new value only if there was an
                    //       actual change, in order to not trigger bevy’s
                    //       change tracking when the values didn’t change.
                    if sleeping.sleeping != rb.is_sleeping() {
                        sleeping.sleeping = rb.is_sleeping();
                    }
                }
            }

            (
                my_new_global_transform,
                parent_delta,
                my_velocity,
                world_offset,
                my_vel_delta,
            )
        } else {
            if let Ok(transform) = q_disabled_trans.get(entity) {
                (
                    *transform,
                    Transform::IDENTITY,
                    Velocity::default(),
                    transform.translation,
                    Velocity::default(),
                )
            } else {
                continue;
            }
        };

        recurse_child_transforms(
            &mut context_access,
            &config,
            &sim_to_render_time,
            timestep_mode.as_ref(),
            &mut writeback,
            transform,
            delta_transform,
            velocity,
            my_vel_delta,
            &children_query,
            entity,
            world_offset,
            &q_disabled_trans,
        );
    }
}

fn recurse_child_transforms(
    context_access: &mut WriteRapierContext,
    config: &Query<&RapierConfiguration>,
    sim_to_render_time: &Query<&SimulationToRenderTime>,
    timestep_mode: &TimestepMode,
    writeback: &mut Query<RigidBodyWritebackComponents, Without<RigidBodyDisabled>>,
    parent_global_transform: Transform,
    parent_delta: Transform,
    parent_velocity: Velocity,
    parent_delta_velocity: Velocity,
    children_query: &Query<&Children>,
    parent_entity: Entity,
    world_offset: Vec3,
    q_disabled_trans: &Query<&Transform, With<RigidBodyDisabled>>,
) {
    let Ok(children) = children_query.get(parent_entity) else {
        return;
    };

    for child in children.iter() {
        let mut world_offset = world_offset;

        let (transform, delta_transform, velocity, delta_velocity) = if let Ok((
            handle,
            link,
            transform,
            rb_type,
            mut interpolation,
            mut velocity,
            mut sleeping,
        )) =
            writeback.get_mut(child)
        {
            let config = config
                .get(link.0)
                .expect("Could not get `RapierConfiguration`");
            if !config.physics_pipeline_active {
                continue;
            }

            let mut my_new_global_transform = parent_global_transform;
            let mut delta_transform = parent_delta;
            let mut my_velocity = parent_velocity;
            let mut my_delta_velocity = parent_delta_velocity;

            let handle = handle.0;

            let rigidbody_set = context_access
                .rapier_context
                .get_mut(link.0)
                .expect("Invalid link")
                .4
                .into_inner();

            let sim_to_render_time = sim_to_render_time
                .get(link.0)
                .expect("Could not get `SimulationToRenderTime`");

            // TODO: do this the other way round: iterate through Rapier’s RigidBodySet on the active bodies,
            // and update the components accordingly. That way, we don’t have to iterate through the entities that weren’t changed
            // by physics (for example because they are sleeping).
            if let Some(rb) = rigidbody_set.bodies.get_mut(handle) {
                let mut interpolated_pos = utils::iso_to_transform(rb.position());

                if let TimestepMode::Interpolated { dt, .. } = *timestep_mode {
                    if let Some(interpolation) = interpolation.as_deref_mut() {
                        if interpolation.end.is_none() {
                            interpolation.end = Some(*rb.position());
                        }

                        if let Some(interpolated) =
                            interpolation.lerp_slerp((dt + sim_to_render_time.diff) / dt)
                        {
                            interpolated_pos = utils::iso_to_transform(&interpolated);
                        }
                    }
                }

                if let Some(mut transform) = transform {
                    // We need to compute the new local transform such that:
                    // curr_parent_global_transform * new_transform * parent_delta_pos = interpolated_pos
                    // new_transform = curr_parent_global_transform.inverse() * interpolated_pos
                    interpolated_pos = interpolated_pos.with_scale(transform.scale);

                    let inverse_parent_rotation = parent_global_transform.rotation.inverse();

                    interpolated_pos.translation -= world_offset;

                    let new_rotation = Quat::IDENTITY; //inverse_parent_rotation * interpolated_pos.rotation;

                    // has to be mut in 2d mode
                    #[allow(unused_mut)]
                    let mut new_translation;

                    let translation_offset =
                        if rb_type.copied().unwrap_or(RigidBody::Fixed) == RigidBody::Dynamic {
                            // The parent's velocity will have already moved them
                            parent_delta.translation
                        } else {
                            Vec3::ZERO
                        };

                    let rotated_interpolation = inverse_parent_rotation
                        * (parent_delta.rotation
                            * (interpolated_pos.translation - translation_offset));

                    new_translation = rotated_interpolation;

                    // In 2D, preserve the transform `z` component that may have been set by the user
                    #[cfg(feature = "dim2")]
                    {
                        new_translation.z = transform.translation.z;
                    }

                    let old_transform = *transform;

                    if transform.rotation != new_rotation
                        || transform.translation != new_translation
                    {
                        // NOTE: we write the new value only if there was an
                        //       actual change, in order to not trigger bevy’s
                        //       change tracking when the values didn’t change.
                        transform.rotation = new_rotation;
                        transform.translation = new_translation;
                    }

                    let inv_old_transform = Transform {
                        scale: old_transform.scale,
                        rotation: old_transform.rotation.inverse(),
                        translation: -old_transform.translation,
                    };

                    delta_transform = transform.mul_transform(inv_old_transform);

                    // NOTE: we need to compute the result of the next transform propagation
                    //       to make sure that our change detection for transforms is exact
                    //       despite rounding errors.

                    my_new_global_transform = parent_global_transform.mul_transform(*transform);
                    world_offset = my_new_global_transform.translation;

                    rigidbody_set
                        .last_body_transform_set
                        .insert(handle, GlobalTransform::from(my_new_global_transform));

                    rb.set_position(utils::transform_to_iso(&my_new_global_transform), false);
                }

                if let Some(velocity) = &mut velocity {
                    let old_linvel = *rb.linvel();

                    my_velocity.linvel = old_linvel.into();

                    rb.set_linvel(parent_velocity.linvel.into(), false);
                    rb.set_linvel(old_linvel - rb.linvel(), false);

                    let mut new_vel = Velocity {
                        linvel: (*rb.linvel()).into(),
                        #[cfg(feature = "dim3")]
                        angvel: (*rb.angvel()).into(),
                        #[cfg(feature = "dim2")]
                        angvel: rb.angvel(),
                    };

                    new_vel.linvel -= parent_delta_velocity.linvel;

                    my_delta_velocity = Velocity {
                        linvel: new_vel.linvel - velocity.linvel,
                        angvel: Default::default(),
                    };

                    // NOTE: we write the new value only if there was an
                    //       actual change, in order to not trigger bevy’s
                    //       change tracking when the values didn’t change.
                    if **velocity != new_vel {
                        **velocity = new_vel;
                    }
                }

                if let Some(sleeping) = &mut sleeping {
                    // NOTE: we write the new value only if there was an
                    //       actual change, in order to not trigger bevy’s
                    //       change tracking when the values didn’t change.
                    if sleeping.sleeping != rb.is_sleeping() {
                        sleeping.sleeping = rb.is_sleeping();
                    }
                }
            }

            (
                my_new_global_transform,
                delta_transform,
                my_velocity,
                my_delta_velocity,
            )
        } else {
            if let Ok(transform) = q_disabled_trans.get(child) {
                (
                    parent_global_transform * *transform,
                    parent_delta,
                    Velocity::default(),
                    Velocity::default(),
                )
            } else {
                continue;
            }
        };

        recurse_child_transforms(
            context_access,
            config,
            sim_to_render_time,
            timestep_mode,
            writeback,
            transform,
            delta_transform,
            velocity,
            delta_velocity,
            children_query,
            child,
            world_offset,
            q_disabled_trans,
        );
    }
}

/// Syncs up child velocities with their parents in the physics simulation.
/// This is done to avoid child components getting hit by their parent and rapier
/// assuming the child is hit by the full velocity of the parent instead of `parent vel - child vel`.
///
/// This will not change the bevy component's velocity.
pub(crate) fn sync_vel(
    top_ents: Query<Entity, Without<ChildOf>>,
    vel_query: Query<&Velocity>,
    query: Query<(&RapierRigidBodyHandle, &RapierContextEntityLink)>,
    children_query: Query<&Children>,
    mut context_access: WriteRapierContext,
) {
    for ent in top_ents.iter() {
        let vel = if let Ok(velocity) = vel_query.get(ent) {
            *velocity
        } else {
            Velocity::default()
        };

        if let Ok(children) = children_query.get(ent) {
            for child in children.iter() {
                sync_velocity_recursively(child, &query, &children_query, vel, &mut context_access);
            }
        }
    }
}

fn sync_velocity_recursively(
    ent: Entity,
    query: &Query<(&RapierRigidBodyHandle, &RapierContextEntityLink)>,
    children_query: &Query<&Children>,
    parent_vel: Velocity,
    context_access: &mut WriteRapierContext,
) {
    let vel = if let Ok((handle, link)) = query.get(ent) {
        let (_, _, _, _, mut context) = context_access
            .rapier_context
            .get_mut(link.0)
            .expect("Invalid link on entity.");

        if let Some(rb) = context.bodies.get_mut(handle.0) {
            #[cfg(feature = "dim3")]
            let old_linvel = Vec3::from(*rb.linvel());
            #[cfg(feature = "dim2")]
            let old_linvel = Vec2::from(*rb.linvel());

            rb.set_linvel((old_linvel + (parent_vel.linvel)).into(), false);

            Velocity {
                linvel: (*rb.linvel()).into(),
                #[cfg(feature = "dim3")]
                angvel: (*rb.angvel()).into(),
                #[cfg(feature = "dim2")]
                angvel: rb.angvel(),
            }
        } else {
            parent_vel
        }
    } else {
        parent_vel
    };

    if let Ok(children) = children_query.get(ent) {
        for child in children.iter() {
            sync_velocity_recursively(child, query, children_query, vel, context_access);
        }
    }
}

/// System responsible for creating new Rapier rigid-bodies from the related `bevy_rapier` components.
pub fn init_rigid_bodies(
    mut commands: Commands,
    default_context_access: Query<Entity, With<DefaultRapierContext>>,
    mut rigidbody_sets: Query<(Entity, &mut RapierRigidBodySet)>,
    rigid_bodies: Query<RigidBodyComponents, Without<RapierRigidBodyHandle>>,
) {
    for (
        (entity, entity_context_link),
        rb,
        transform,
        vel,
        additional_mass_props,
        _mass_props,
        locked_axes,
        force,
        gravity_scale,
        (ccd, soft_ccd),
        dominance,
        sleep,
        damping,
        disabled,
        additional_solver_iters,
    ) in rigid_bodies.iter()
    {
        let mut builder = RigidBodyBuilder::new((*rb).into());
        builder = builder.enabled(disabled.is_none());

        if let Some(transform) = transform {
            builder = builder.position(utils::transform_to_iso(&transform.compute_transform()));
        }

        #[allow(clippy::useless_conversion)] // Need to convert if dim3 enabled
        if let Some(vel) = vel {
            builder = builder.linvel(vel.linvel.into()).angvel(vel.angvel.into());
        }

        if let Some(locked_axes) = locked_axes {
            builder = builder.locked_axes((*locked_axes).into())
        }

        if let Some(gravity_scale) = gravity_scale {
            builder = builder.gravity_scale(gravity_scale.0);
        }

        if let Some(ccd) = ccd {
            builder = builder.ccd_enabled(ccd.enabled)
        }

        if let Some(soft_ccd) = soft_ccd {
            builder = builder.soft_ccd_prediction(soft_ccd.prediction)
        }

        if let Some(dominance) = dominance {
            builder = builder.dominance_group(dominance.groups)
        }

        if let Some(sleep) = sleep {
            builder = builder.sleeping(sleep.sleeping);
        }

        if let Some(damping) = damping {
            builder = builder
                .linear_damping(damping.linear_damping)
                .angular_damping(damping.angular_damping);
        }

        if let Some(mprops) = additional_mass_props {
            builder = match mprops {
                AdditionalMassProperties::MassProperties(mprops) => {
                    builder.additional_mass_properties(mprops.into_rapier())
                }
                AdditionalMassProperties::Mass(mass) => builder.additional_mass(*mass),
            };
        }

        if let Some(added_iters) = additional_solver_iters {
            builder = builder.additional_solver_iterations(added_iters.0);
        }

        builder = builder.user_data(entity.to_bits() as u128);

        let mut rb = builder.build();

        #[allow(clippy::useless_conversion)] // Need to convert if dim3 enabled
        if let Some(force) = force {
            rb.add_force(force.force.into(), false);
            rb.add_torque(force.torque.into(), false);
        }

        // NOTE: we can’t apply impulses yet at this point because
        //       the rigid-body’s mass isn’t up-to-date yet (its
        //       attached colliders, if any, haven’t been created yet).

        if let Some(sleep) = sleep {
            let activation = rb.activation_mut();
            activation.normalized_linear_threshold = sleep.normalized_linear_threshold;
            activation.angular_threshold = sleep.angular_threshold;
        }
        // Get rapier context from RapierContextEntityLink or insert its default value.
        let context_entity = entity_context_link.map_or_else(
            || {
                let context_entity = default_context_access.single().ok()?;
                commands
                    .entity(entity)
                    .insert(RapierContextEntityLink(context_entity));
                Some(context_entity)
            },
            |link| Some(link.0),
        );
        let Some(context_entity) = context_entity else {
            continue;
        };

        let Ok((_, mut rigidbody_set)) = rigidbody_sets.get_mut(context_entity) else {
            log::error!("Could not find entity {context_entity} with rapier context while initializing {entity}");
            continue;
        };
        let handle = rigidbody_set.bodies.insert(rb);
        commands
            .entity(entity)
            .insert(RapierRigidBodyHandle(handle));
        rigidbody_set.entity2body.insert(entity, handle);

        if let Some(transform) = transform {
            rigidbody_set
                .last_body_transform_set
                .insert(handle, *transform);
        }
    }
}

/// This applies the initial impulse given to a rigid-body when it is created.
///
/// This cannot be done inside `init_rigid_bodies` because impulses require the rigid-body
/// mass to be available, which it was not because colliders were not created yet. As a
/// result, we run this system after the collider creation.
pub fn apply_initial_rigid_body_impulses(
    mut context: Query<(&mut RapierRigidBodySet, &RapierContextColliders)>,
    // We can’t use RapierRigidBodyHandle yet because its creation command hasn’t been
    // executed yet.
    mut init_impulses: Query<
        (Entity, &RapierContextEntityLink, &mut ExternalImpulse),
        Without<RapierRigidBodyHandle>,
    >,
) {
    for (entity, link, mut impulse) in init_impulses.iter_mut() {
        let (mut rigidbody_set, context_colliders) =
            context.get_mut(link.0).expect(RAPIER_CONTEXT_EXPECT_ERROR);
        let rigidbody_set = &mut *rigidbody_set;

        let bodies = &mut rigidbody_set.bodies;
        if let Some(rb) = rigidbody_set
            .entity2body
            .get(&entity)
            .and_then(|h| bodies.get_mut(*h))
        {
            // Make sure the mass-properties are computed.
            rb.recompute_mass_properties_from_colliders(&context_colliders.colliders);
            // Apply the impulse.
            rb.apply_impulse(impulse.impulse.into(), false);

            #[allow(clippy::useless_conversion)] // Need to convert if dim3 enabled
            rb.apply_torque_impulse(impulse.torque_impulse.into(), false);

            impulse.reset();
        }
    }
}
