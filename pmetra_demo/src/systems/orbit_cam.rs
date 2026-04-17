use bevy::{
    input::{
        mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
        touch::Touches,
    },
    prelude::*,
};
use bevy_rapier3d::prelude::*;
use smooth_bevy_cameras::{
    controllers::orbit::{ControlMessage, OrbitCameraController},
    LookTransform,
};

pub fn orbit_cam_custom_input_map_controller(
    mut events: MessageWriter<ControlMessage>,
    mut mouse_wheel_reader: MessageReader<MouseWheel>,
    mut mouse_motion_events: MessageReader<MouseMotion>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    touches: Res<Touches>,
    controllers: Query<&OrbitCameraController>,
) {
    // Can only control one camera at a time.
    let controller = if let Some(controller) = controllers.iter().find(|c| c.enabled) {
        controller
    } else {
        return;
    };
    let OrbitCameraController {
        mouse_rotate_sensitivity,
        mouse_translate_sensitivity,
        mouse_wheel_zoom_sensitivity,
        pixels_per_line,
        ..
    } = *controller;

    // ── Mouse input (desktop) ─────────────────────────────────────────────
    let mut cursor_delta = Vec2::ZERO;
    for event in mouse_motion_events.read() {
        cursor_delta += event.delta;
    }

    if mouse_buttons.pressed(MouseButton::Right) {
        if keyboard.pressed(KeyCode::ShiftLeft) {
            events.write(ControlMessage::TranslateTarget(
                mouse_translate_sensitivity * cursor_delta,
            ));
        } else {
            events.write(ControlMessage::Orbit(
                mouse_rotate_sensitivity * cursor_delta,
            ));
        }
    }

    let mut scalar = 1.0;
    for event in mouse_wheel_reader.read() {
        // scale the event magnitude per pixel or per line
        let scroll_amount = match event.unit {
            MouseScrollUnit::Line => event.y,
            MouseScrollUnit::Pixel => event.y / pixels_per_line,
        };
        scalar *= 1.0 - scroll_amount * mouse_wheel_zoom_sensitivity;
    }

    // ── Touch input (mobile) ──────────────────────────────────────────────
    let active_touches: Vec<_> = touches.iter().collect();
    match active_touches.len() {
        1 => {
            // Single finger drag → orbit
            let delta = active_touches[0].delta();
            if delta != Vec2::ZERO {
                events.write(ControlMessage::Orbit(
                    mouse_rotate_sensitivity * delta,
                ));
            }
        }
        2 => {
            let t0 = active_touches[0];
            let t1 = active_touches[1];

            // Two-finger drag → pan (average of both deltas)
            let avg_delta = (t0.delta() + t1.delta()) / 2.0;
            if avg_delta != Vec2::ZERO {
                events.write(ControlMessage::TranslateTarget(
                    mouse_translate_sensitivity * avg_delta,
                ));
            }

            // Pinch → zoom (change in distance between fingers)
            let prev_dist = (t0.previous_position() - t1.previous_position()).length();
            let cur_dist = (t0.position() - t1.position()).length();
            if prev_dist > 0.0 {
                let pinch_ratio = prev_dist / cur_dist;
                scalar *= pinch_ratio;
            }
        }
        _ => {}
    }

    events.write(ControlMessage::Zoom(scalar));
}

const IMPULSE_MAG: f32 = 0.0007;
const BULLET_SPHERE_RADIUS: f32 = 0.03;

pub fn fire_balls_at_look_point(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    query_orbit_cam: Query<&LookTransform, With<OrbitCameraController>>,
) {
    if !keyboard_input.just_pressed(KeyCode::Space) {
        return;
    }
    let Ok(LookTransform { eye, target, .. }) = query_orbit_cam.single() else {
        return;
    };

    let impulse_dir = (*target - *eye).normalize();
    let ext_impulse = ExternalImpulse {
        impulse: impulse_dir * IMPULSE_MAG,
        ..default()
    };

    debug!("Spawning bullet ball...");
    // Spawn bullet ball...
    commands.spawn((
        Mesh3d(meshes.add(Sphere {
            radius: BULLET_SPHERE_RADIUS,
        })),
        MeshMaterial3d(materials.add(Color::WHITE)),
        Transform::from_translation(*eye),
        RigidBody::Dynamic,
        Collider::ball(BULLET_SPHERE_RADIUS),
        Ccd::enabled(),
        ext_impulse,
    ));
}
