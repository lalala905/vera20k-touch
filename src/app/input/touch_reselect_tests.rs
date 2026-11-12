//! Touch group-tap: a finger on a unit the current multi-unit selection
//! already holds replaces the selection with it, where the native multi-select
//! click orders the rest of the group to move near the tapped one. The decision
//! half is tested here; the state half only adds the touch gate and the pointer
//! pick.

use super::touch_reselects_selected_unit_parts;
use crate::map::entities::EntityCategory;
use crate::sim::combat::Health;
use crate::sim::game_entity::GameEntity;
use crate::sim::world::Simulation;

fn scene(category: EntityCategory, ids: &[u64]) -> Simulation {
    let mut sim = Simulation::new();
    let owner = sim.intern("viewer");
    let typ = sim.intern("MTNK");
    for id in ids {
        let entity = GameEntity::new_at_frame_zero_for_test(
            *id,
            10,
            10,
            128,
            128,
            owner,
            Health::new(100),
            typ,
            category,
            0,
            5,
            false,
        );
        sim.entities_mut_for_test().insert(entity);
    }
    sim
}

#[test]
fn a_touch_tap_on_a_group_member_selects_just_it() {
    let sim = scene(EntityCategory::Unit, &[1, 2]);
    let current = [1, 2];
    assert!(
        touch_reselects_selected_unit_parts(&current, Some(1), &sim, "viewer"),
        "a finger on a selected unit means just that unit"
    );
}

#[test]
fn mouse_and_plain_clicks_keep_every_native_answer() {
    let sim = scene(EntityCategory::Unit, &[1, 2, 3]);
    let group = [1, 2];
    // A single-unit selection keeps the native click (self-deploy and orders).
    assert!(!touch_reselects_selected_unit_parts(
        &group[..1],
        Some(1),
        &sim,
        "viewer"
    ));
    // A unit outside the selection is an ordinary click, not a reselect.
    assert!(!touch_reselects_selected_unit_parts(
        &group,
        Some(3),
        &sim,
        "viewer"
    ));
    // Nothing under the finger.
    assert!(!touch_reselects_selected_unit_parts(
        &group, None, &sim, "viewer"
    ));
    // Another player's unit is never this player's reselect target.
    assert!(!touch_reselects_selected_unit_parts(
        &group,
        Some(1),
        &sim,
        "peer"
    ));
}

#[test]
fn a_structure_is_never_a_touch_reselect() {
    let sim = scene(EntityCategory::Structure, &[1, 2]);
    let current = [1, 2];
    assert!(!touch_reselects_selected_unit_parts(
        &current,
        Some(1),
        &sim,
        "viewer"
    ));
}
