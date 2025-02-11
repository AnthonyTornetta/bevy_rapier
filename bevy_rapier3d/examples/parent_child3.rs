use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

fn main() {
    App::new()
        .insert_resource(ClearColor(
            Srgba {
                red: 0xF9 as f32 / 255.0,
                green: 0xF9 as f32 / 255.0,
                blue: 0xFF as f32 / 255.0,
                alpha: 1.0,
            }
            .into(),
        ))
        .add_plugins(DefaultPlugins)
        .add_plugins(RapierPhysicsPlugin::<NoUserData>::default())
        .add_plugins(RapierDebugRenderPlugin::default())
        .add_systems(Startup, setup_simulation)
        .add_systems(Update, print_vel)
        .run();
}

const VEL_Y: f32 = 2.0;
const TOP_CUBE_DIFF_VEL: f32 = -1.0;

fn setup_simulation(mut commands: Commands) {
    // Right side - parent child entities are not working as expected

    commands
        .spawn((
            Transform::from_xyz(1.0, 0.0, 0.0),
            RigidBody::Dynamic,
            Collider::cuboid(0.5, 0.5, 0.5),
            ColliderDebugColor(Hsla {
                hue: 180.0,
                saturation: 1.0,
                lightness: 0.3,
                alpha: 1.0,
            }),
            Velocity::linear(Vec3::new(0.0, VEL_Y, 0.0)),
            GravityScale(0.0),
            Ccd::enabled(),
        ))
        .with_children(|child| {
            child
                .spawn((
                    Transform::from_xyz(0.0, 5.0, 0.0),
                    RigidBody::Dynamic,
                    Collider::cuboid(0.5, 0.5, 0.5),
                    ColliderDebugColor(Hsla {
                        hue: 220.0,
                        saturation: 1.0,
                        lightness: 0.3,
                        alpha: 1.0,
                    }),
                    Ccd::enabled(),
                    Velocity::linear(Vec3::new(0.0, TOP_CUBE_DIFF_VEL, 0.0)),
                    GravityScale(0.0),
                ))
                .with_children(|child| {
                    child.spawn((
                        Camera3d::default(),
                        Transform::from_xyz(-1.0, 10.0, 10.0)
                            .looking_at(Vec3::new(-1.0, 0.0, 0.0), Vec3::Y),
                    ));
                });
        });

    // Left side - independent entities are fine
    // This does the simulation correctly

    commands.spawn((
        Transform::from_xyz(-1.0, 0.0, 0.0),
        RigidBody::Dynamic,
        Collider::cuboid(0.5, 0.5, 0.5),
        ColliderDebugColor(Hsla {
            hue: 180.0,
            saturation: 1.0,
            lightness: 0.3,
            alpha: 1.0,
        }),
        Velocity::linear(Vec3::new(0.0, VEL_Y, 0.0)),
        GravityScale(0.0),
        Ccd::enabled(),
    ));

    commands.spawn((
        Transform::from_xyz(-1.0, 5.0, 0.0),
        RigidBody::Dynamic,
        Collider::cuboid(0.5, 0.5, 0.5),
        ColliderDebugColor(Hsla {
            hue: 220.0,
            saturation: 1.0,
            lightness: 0.3,
            alpha: 1.0,
        }),
        Ccd::enabled(),
        Velocity::linear(Vec3::new(0.0, VEL_Y + TOP_CUBE_DIFF_VEL, 0.0)),
        GravityScale(0.0),
    ));
}

fn print_vel(query: Query<(&Transform, &Velocity)>) {
    println!("=====");
    for (transform, vel) in query.iter() {
        println!("{}: {}", transform.translation.y, vel.linvel);
    }
}
