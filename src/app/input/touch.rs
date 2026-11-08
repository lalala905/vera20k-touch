//! Touch → mouse translation for touchscreens.
//!
//! RA2's UI is a mouse UI: every screen reads the cursor position and the
//! button edges through the same `CursorMoved`/`MouseInput` paths the OS mouse
//! drives. A touchscreen has no cursor, and Windows delivers its contacts as
//! `WindowEvent::Touch` only (winit registers the window for touch and consumes
//! `WM_TOUCH`, which stops the system's mouse synthesis), so this mapper
//! re-issues each touch as the gesture it means.
//!
//! The gesture set follows Rusted Warfare's Android port, so a hand that knows
//! that game finds the same controls here:
//!
//! - a quick release clicks the left button — select, order, UI;
//! - a one-finger drag that started over the battlefield pans the camera, the
//!   map following the finger (grab style), reported as a `pan` displacement
//!   for the caller to apply — native RA2's right-drag pan is a joystick that
//!   keeps gliding while the finger rests, which overshoots badly on a
//!   touchscreen, and its release can fire the right-click cancel ladder;
//! - a one-finger drag that started over the sidebar or a menu keeps the
//!   left button held from the touch origin, which is RA2's own slider/list
//!   drag there (the select box needs two fingers now, see below);
//! - two fingers dragged over the battlefield draw RA2's select box: the box
//!   spans the rectangle the pair holds, both edges following the contacts, so
//!   it grows left and up as well as right and down;
//! - two fingers tapped without moving cancel the selection, reported as
//!   `deselect` for the caller to run through the selection owner;
//! - a long, still touch fires the right button — RA2's cancel/context action.
//!
//! A touch first only moves the cursor: the button decision waits until the
//! gesture is unambiguous. While a touch drives the pointer the game hides
//! mouse.shp (`AppState::software_cursor_drawn`); the synthetic cursor still
//! feeds the hit tests under the finger.
//!
//! A third finger is ignored, and so is a second finger outside a battlefield
//! gesture: both would otherwise steal the gesture the hand is already making.
//! A contact that lands on top of a tracked finger is a second delivery of that
//! finger, not a new one (`DUPLICATE_CONTACT_PX`).
//!
//! The call site drives this on every host except Linux, where the exclusion is
//! X11-only: those desktops keep driving the pointer from touch, so translating
//! again would double every click. A Wayland touchscreen delivers `wl_touch`
//! with no pointer events to fall back on, so it gets no input here. macOS has
//! no touchscreen. Capture sessions are input-sealed everywhere: they keep
//! counting raw touches as violations.
//!
//! The mapper owns no game state and never reaches the simulation. `translate`
//! takes the current `Instant`, whether the contact landed over the battlefield
//! and the window scale factor as parameters so every boundary is testable
//! without a clock or a running app.
//
// The module stays compiled on every host so its gestures keep their tests
// there; only the call site is Windows-bound.
#![cfg_attr(target_os = "linux", allow(dead_code))]

use crate::util::time::{Duration, Instant};

use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, MouseButton, Touch, TouchPhase, WindowEvent};

/// A pending touch held this long without leaving its origin becomes a right
/// click immediately.
const LONG_PRESS: Duration = Duration::from_millis(400);
/// Movement beyond this many logical pixels from the touch origin upgrades a
/// pending touch: a one-finger battlefield drag becomes a camera pan, a UI drag
/// holds the left button, and a two-finger drag starts the select box.
/// Rusted Warfare's touch threshold. Multiplied by the window scale factor so
/// the hand travel stays the same on every display density.
const DRAG_THRESHOLD_LOGICAL_PX: f64 = 15.0;
/// A contact this close to a tracked finger repeats it rather than adding one.
/// winit 0.30 reissues every Windows touch from two paths under two ids
/// (`WM_TOUCH`'s `TOUCHINPUT.dwID` and `WM_POINTER*`'s `pointerId`), so one
/// physical finger can arrive twice; taking the copy as a second finger would
/// start a select box on every tap. Two fingers cannot share a point.
const DUPLICATE_CONTACT_PX: f64 = 2.0;
/// Fingers tracked at once: a lone finger drives the cursor, a pair drives the
/// select box.
const MAX_FINGERS: usize = 2;
/// Synthetic events one touch can mean. The longest step is a click: move to
/// the point, press, release.
const MAX_SYNTHETICS: usize = 3;

/// Synthetic events for one touch, in dispatch order.
pub(crate) type TouchSynthetics = [Option<WindowEvent>; MAX_SYNTHETICS];

/// One touch translated into what the app should do with it.
#[derive(Debug, Default)]
pub(crate) struct TouchOutput {
    /// Mouse events to dispatch, in order.
    pub events: TouchSynthetics,
    /// Camera displacement in screen pixels, grab style: the camera moves by
    /// this much so the map follows the finger. The caller divides by the
    /// current zoom to reach world pixels. `None` unless a one-finger
    /// battlefield drag is moving the map.
    pub pan: Option<(f32, f32)>,
    /// The two-finger tap landed: cancel the selection. The caller runs the
    /// selection owner's own clear (`Unselect_All_With_Mode_Reset 0x0048DC90`)
    /// instead of synthesizing a click, so a tap over an object cancels the
    /// selection rather than selecting that object.
    pub deselect: bool,
    /// A live two-finger select box's current top-left corner, in window
    /// pixels. The handler re-anchors the selection drag to it on every move,
    /// so the box follows both edges of the pair and can grow left and up as
    /// well as right and down. `None` unless a two-finger box is being drawn.
    pub band_anchor: Option<(f32, f32)>,
}

impl TouchOutput {
    fn events(events: TouchSynthetics) -> Self {
        Self {
            events,
            ..Self::default()
        }
    }
}

/// One tracked finger's last known position and where it landed.
#[derive(Clone, Copy, Debug)]
struct Finger {
    id: u64,
    pos: PhysicalPosition<f64>,
    /// Where this contact landed. A two-finger box starts once either contact
    /// travels past the drag threshold from here.
    start: PhysicalPosition<f64>,
}

/// The gesture the tracked finger set is currently driving.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    /// One finger down, no button synthesized yet; the cursor follows.
    Pending,
    /// A synthesized left-button drag over the UI: a slider or list drag.
    UiDrag,
    /// A one-finger camera pan over the battlefield; `last` is the previous
    /// finger position.
    Pan { last: PhysicalPosition<f64> },
    /// Two fingers down drawing the select box; `band` is set once the pair
    /// moved far enough to press the left button. The box's corners both follow
    /// the pair, re-anchored through `TouchOutput::band_anchor` on each move.
    TwoFinger { band: bool },
    /// A long-press right click fired, or the gesture ended; the touch's
    /// remaining events are consumed.
    Done,
}

/// Translates winit touch events into the synthetic mouse events they mean.
#[derive(Debug)]
pub(crate) struct TouchMouseMapper {
    /// Tracked fingers, oldest first. A lifted finger leaves a hole, and the
    /// next contact fills it, so a live finger is never overwritten.
    fingers: [Option<Finger>; MAX_FINGERS],
    gesture: Gesture,
    /// The pending finger's origin, kept for the long-press stillness check and
    /// for the pan's starting point.
    origin: PhysicalPosition<f64>,
    /// Whether the current gesture's first contact landed over the battlefield.
    /// A battlefield drag pans the camera; a UI drag keeps the left button.
    origin_over_battlefield: bool,
    started: Option<Instant>,
}

impl TouchMouseMapper {
    pub(crate) const fn new() -> Self {
        Self {
            fingers: [None, None],
            gesture: Gesture::Pending,
            origin: PhysicalPosition::new(0.0, 0.0),
            origin_over_battlefield: false,
            started: None,
        }
    }

    /// Drop every tracked touch. A stream can end without an `Ended` — the
    /// contact is cancelled, or the window loses focus with fingers down — and
    /// a phantom finger would turn every later tap into a two-finger gesture.
    pub(crate) fn reset(&mut self) {
        self.fingers = [None; MAX_FINGERS];
        self.gesture = Gesture::Pending;
        self.origin = PhysicalPosition::new(0.0, 0.0);
        self.origin_over_battlefield = false;
        self.started = None;
    }

    /// Whether a touch camera gesture owns the camera right now. The app
    /// inhibits edge auto-scroll while it does, standing in for the tactical
    /// capture native's right-drag pan holds.
    pub(crate) fn pan_active(&self) -> bool {
        matches!(
            self.gesture,
            Gesture::Pan { .. } | Gesture::TwoFinger { .. }
        )
    }

    /// Map one touch into the events and camera motion it means.
    /// `over_battlefield` is whether this contact landed over the tactical
    /// viewport; it decides a one-finger drag's meaning and admits the second
    /// finger of a select box. `scale` is the window's scale factor, which
    /// scales the drag threshold.
    pub(crate) fn translate(
        &mut self,
        touch: &Touch,
        now: Instant,
        over_battlefield: bool,
        scale: f64,
    ) -> TouchOutput {
        match touch.phase {
            TouchPhase::Started => self.on_finger_down(touch, now, over_battlefield),
            TouchPhase::Moved => {
                self.finger_moved(touch);
                self.on_finger_move(touch, now, scale)
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                // The lift's own position is part of a box's final rectangle,
                // so record it before the release resolves.
                self.finger_moved(touch);
                let output = self.on_finger_up(touch, now, scale);
                self.finger_remove(touch.id);
                output
            }
        }
    }

    // --- finger table ---------------------------------------------------

    fn tracked(&self) -> usize {
        self.fingers.iter().flatten().count()
    }

    fn finger_tracked(&self, id: u64) -> bool {
        self.fingers.iter().flatten().any(|finger| finger.id == id)
    }

    fn finger_moved(&mut self, touch: &Touch) {
        for slot in &mut self.fingers {
            if let Some(finger) = slot
                && finger.id == touch.id
            {
                finger.pos = touch.location;
            }
        }
    }

    /// Remove a finger and keep the table dense, so the next touch takes the
    /// free slot instead of overwriting a live finger.
    fn finger_remove(&mut self, id: u64) {
        let mut dense = [None; MAX_FINGERS];
        for (slot, finger) in self
            .fingers
            .iter()
            .flatten()
            .filter(|finger| finger.id != id)
            .enumerate()
        {
            dense[slot] = Some(*finger);
        }
        self.fingers = dense;
    }

    fn centroid(&self) -> PhysicalPosition<f64> {
        let mut sum = (0.0, 0.0);
        for finger in self.fingers.iter().flatten() {
            sum.0 += finger.pos.x;
            sum.1 += finger.pos.y;
        }
        let count = self.tracked().max(1) as f64;
        PhysicalPosition::new(sum.0 / count, sum.1 / count)
    }

    /// The rectangle the tracked fingers span: its top-left and bottom-right
    /// corners. The select box runs between them, so the box covers exactly
    /// the area the hand holds, not the centroid's travel.
    fn finger_box(&self) -> Option<(PhysicalPosition<f64>, PhysicalPosition<f64>)> {
        let mut fingers = self.fingers.iter().flatten();
        let first = fingers.next()?;
        let (mut min, mut max) = (first.pos, first.pos);
        for finger in fingers {
            min.x = min.x.min(finger.pos.x);
            min.y = min.y.min(finger.pos.y);
            max.x = max.x.max(finger.pos.x);
            max.y = max.y.max(finger.pos.y);
        }
        Some((min, max))
    }

    /// A replacement contact only rejoins a battlefield gesture that can
    /// still become (or stay) the select box. A UI drag keeps its button and a
    /// finished gesture stays finished.
    fn accepts_second_finger(&self) -> bool {
        self.origin_over_battlefield
            && matches!(
                self.gesture,
                Gesture::Pending | Gesture::Pan { .. } | Gesture::TwoFinger { band: false, .. }
            )
    }

    // --- gesture edges --------------------------------------------------

    fn on_finger_down(
        &mut self,
        touch: &Touch,
        now: Instant,
        over_battlefield: bool,
    ) -> TouchOutput {
        let tracked_before = self.tracked();
        // A copy of a tracked contact, not a new finger (see
        // `DUPLICATE_CONTACT_PX`); it stays untracked, so its moves and its
        // lift fall through the guards below.
        if self
            .fingers
            .iter()
            .flatten()
            .any(|finger| distance(finger.pos, touch.location) <= DUPLICATE_CONTACT_PX)
        {
            return TouchOutput::default();
        }
        if tracked_before >= MAX_FINGERS {
            return TouchOutput::default();
        }
        // A second finger only drives a battlefield gesture when it also
        // landed over the battlefield: two contacts inside a convex viewport
        // span a rectangle inside it, so the box's press cannot land on a
        // sidebar gadget.
        if tracked_before == 1 && !(self.accepts_second_finger() && over_battlefield) {
            return TouchOutput::default();
        }
        let Some(slot) = self.fingers.iter().position(Option::is_none) else {
            return TouchOutput::default();
        };
        self.fingers[slot] = Some(Finger {
            id: touch.id,
            pos: touch.location,
            start: touch.location,
        });
        let events = if tracked_before == 0 {
            // First finger: start pending and follow the cursor.
            self.gesture = Gesture::Pending;
            self.origin = touch.location;
            self.origin_over_battlefield = over_battlefield;
            self.started = Some(now);
            [Some(Self::cursor_moved(touch)), None, None]
        } else {
            // Second finger: the pair becomes the select-box gesture. No button
            // yet — the pair only presses once a contact has moved past the
            // threshold, so a two-finger tap stays a tap. A contact that
            // replaces one finger of an undecided pair keeps the gesture, and
            // every contact's travel baseline restarts here.
            for finger in self.fingers.iter_mut().flatten() {
                finger.start = finger.pos;
            }
            let centroid = self.centroid();
            if !matches!(self.gesture, Gesture::TwoFinger { band: false }) {
                self.gesture = Gesture::TwoFinger { band: false };
            }
            [Some(Self::cursor_at(touch.device_id, centroid)), None, None]
        };
        TouchOutput::events(events)
    }

    fn on_finger_move(&mut self, touch: &Touch, now: Instant, scale: f64) -> TouchOutput {
        if !self.finger_tracked(touch.id) {
            return TouchOutput::default();
        }
        match self.gesture {
            Gesture::Done => TouchOutput::default(),
            Gesture::Pending => {
                let threshold = drag_threshold(scale);
                let held = self
                    .started
                    .is_some_and(|start| now.duration_since(start) >= LONG_PRESS);
                if held && distance(touch.location, self.origin) <= threshold {
                    // Long press: fire the context click right away.
                    self.gesture = Gesture::Done;
                    return TouchOutput::events(Self::click_at(touch, MouseButton::Right));
                }
                if distance(touch.location, self.origin) > threshold {
                    if self.origin_over_battlefield {
                        // The battlefield drag is the camera: the map follows
                        // the finger from where it landed, so the camera moves
                        // against it.
                        self.gesture = Gesture::Pan {
                            last: touch.location,
                        };
                        return TouchOutput {
                            pan: Some((
                                (self.origin.x - touch.location.x) as f32,
                                (self.origin.y - touch.location.y) as f32,
                            )),
                            ..Default::default()
                        };
                    }
                    // Over the sidebar or a menu the same drag is the
                    // left-button drag a slider or list means, so the press
                    // lands on the grabbed origin, not the moved finger.
                    self.gesture = Gesture::UiDrag;
                    return TouchOutput::events([
                        Some(Self::cursor_at(touch.device_id, self.origin)),
                        Some(Self::mouse_input(
                            touch,
                            ElementState::Pressed,
                            MouseButton::Left,
                        )),
                        None,
                    ]);
                }
                // Still pending: the cursor follows the finger.
                TouchOutput::events([Some(Self::cursor_moved(touch)), None, None])
            }
            // The slider/list drag follows the finger.
            Gesture::UiDrag => TouchOutput::events([Some(Self::cursor_moved(touch)), None, None]),
            // The camera follows the finger: the map moves with the hand, so
            // the camera moves against it.
            Gesture::Pan { last } => {
                self.gesture = Gesture::Pan {
                    last: touch.location,
                };
                TouchOutput {
                    pan: Some((
                        (last.x - touch.location.x) as f32,
                        (last.y - touch.location.y) as f32,
                    )),
                    ..Default::default()
                }
            }
            // The select box follows both edges of the pair: it spans their
            // current rectangle, and `band_anchor` re-anchors the drag's fixed
            // corner to that rectangle's top-left on every move.
            Gesture::TwoFinger { band } => {
                let Some((left_top, bottom_right)) = self.finger_box() else {
                    return TouchOutput::default();
                };
                if !band {
                    // A pair that is already down to one finger is a two-finger
                    // tap in progress, not a box: the survivor's travel must not
                    // open the box. Opening it takes a contact that left its
                    // landing position.
                    if self.tracked() < 2
                        || !self.fingers.iter().flatten().any(|finger| {
                            distance(finger.pos, finger.start) > drag_threshold(scale)
                        })
                    {
                        return TouchOutput::default();
                    }
                    self.gesture = Gesture::TwoFinger { band: true };
                    return TouchOutput {
                        events: [
                            Some(Self::cursor_at(touch.device_id, left_top)),
                            Some(Self::mouse_input(
                                touch,
                                ElementState::Pressed,
                                MouseButton::Left,
                            )),
                            Some(Self::cursor_at(touch.device_id, bottom_right)),
                        ],
                        band_anchor: Some((left_top.x as f32, left_top.y as f32)),
                        ..Default::default()
                    };
                }
                TouchOutput {
                    events: [
                        Some(Self::cursor_at(touch.device_id, bottom_right)),
                        None,
                        None,
                    ],
                    band_anchor: Some((left_top.x as f32, left_top.y as f32)),
                    ..Default::default()
                }
            }
        }
    }

    fn on_finger_up(&mut self, touch: &Touch, now: Instant, scale: f64) -> TouchOutput {
        if !self.finger_tracked(touch.id) {
            return TouchOutput::default();
        }
        let remaining = self.tracked() - 1;
        match self.gesture {
            Gesture::Done => TouchOutput::default(),
            Gesture::Pending => {
                // Read the start before clearing it: the fallback below needs
                // the real hold time.
                let start = self.pending_started(now);
                self.gesture = Gesture::Done;
                self.started = None;
                // A cancelled contact is not a click.
                if touch.phase != TouchPhase::Ended {
                    return TouchOutput::default();
                }
                let threshold = drag_threshold(scale);
                let held = now.duration_since(start) >= LONG_PRESS;
                let still = distance(touch.location, self.origin) <= threshold;
                let button = if held && still {
                    // Long-press fallback for coarse event delivery: the live
                    // check in the move handler usually fires first.
                    MouseButton::Right
                } else {
                    // A quick release clicks.
                    MouseButton::Left
                };
                TouchOutput::events(Self::click_at(touch, button))
            }
            Gesture::UiDrag => {
                // The UI drag is single-finger, so the second contact never
                // joined and its own lift ends it.
                self.gesture = Gesture::Done;
                TouchOutput::events([
                    Some(Self::cursor_at(touch.device_id, touch.location)),
                    Some(Self::mouse_input(
                        touch,
                        ElementState::Released,
                        MouseButton::Left,
                    )),
                    None,
                ])
            }
            Gesture::Pan { .. } => {
                // No button was ever synthesized: lifting simply ends the pan.
                self.gesture = Gesture::Done;
                TouchOutput::default()
            }
            Gesture::TwoFinger { band: true, .. } => {
                // The box ends at the pair's final rectangle: the lifted
                // contact's final position is part of it, and the fixed corner
                // is re-anchored to the final top-left.
                let Some((left_top, bottom_right)) = self.finger_box() else {
                    self.gesture = Gesture::Done;
                    return TouchOutput::default();
                };
                self.gesture = Gesture::Done;
                TouchOutput {
                    events: [
                        Some(Self::cursor_at(touch.device_id, bottom_right)),
                        Some(Self::mouse_input(
                            touch,
                            ElementState::Released,
                            MouseButton::Left,
                        )),
                        None,
                    ],
                    band_anchor: Some((left_top.x as f32, left_top.y as f32)),
                    ..Default::default()
                }
            }
            Gesture::TwoFinger { band: false, .. } if remaining == 0 => {
                // Neither finger ever moved past the threshold: a two-finger
                // tap cancels the selection.
                self.gesture = Gesture::Done;
                TouchOutput {
                    deselect: touch.phase == TouchPhase::Ended,
                    ..Default::default()
                }
            }
            Gesture::TwoFinger { band: false, .. } => {
                // One finger of the tap lifted: keep the gesture so the
                // survivor's own lift finishes the tap, and ignore its travel
                // until then.
                TouchOutput::default()
            }
        }
    }

    /// The pending start instant, treating a lost clock as "just started".
    fn pending_started(&self, now: Instant) -> Instant {
        self.started.unwrap_or(now)
    }

    // --- synthetic events -------------------------------------------------

    fn cursor_at(device_id: DeviceId, position: PhysicalPosition<f64>) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id,
            position,
        }
    }

    fn cursor_moved(touch: &Touch) -> WindowEvent {
        Self::cursor_at(touch.device_id, touch.location)
    }

    fn mouse_input(touch: &Touch, state: ElementState, button: MouseButton) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: touch.device_id,
            state,
            button,
        }
    }

    /// A press and release of `button` where the finger lifted.
    fn click_at(touch: &Touch, button: MouseButton) -> TouchSynthetics {
        [
            Some(Self::cursor_at(touch.device_id, touch.location)),
            Some(Self::mouse_input(touch, ElementState::Pressed, button)),
            Some(Self::mouse_input(touch, ElementState::Released, button)),
        ]
    }
}

fn drag_threshold(scale: f64) -> f64 {
    DRAG_THRESHOLD_LOGICAL_PX * scale.max(1.0)
}

fn distance(a: PhysicalPosition<f64>, b: PhysicalPosition<f64>) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::Force;

    fn touch(id: u64, phase: TouchPhase, x: f64, y: f64) -> Touch {
        Touch {
            device_id: DeviceId::dummy(),
            phase,
            location: PhysicalPosition::new(x, y),
            force: Some(Force::Calibrated {
                force: 1.0,
                max_possible_force: 1.0,
                altitude_angle: None,
            }),
            id,
        }
    }

    /// Render synthetic mouse events as short names, e.g. "move", "Pressed Left".
    fn names(output: TouchOutput) -> Vec<String> {
        output
            .events
            .into_iter()
            .flatten()
            .map(|event| match event {
                WindowEvent::CursorMoved { .. } => "move".to_owned(),
                WindowEvent::MouseInput { state, button, .. } => format!(
                    "{:?} {}",
                    state,
                    match button {
                        MouseButton::Left => "Left",
                        MouseButton::Right => "Right",
                        _ => "Other",
                    }
                ),
                _ => "other".to_owned(),
            })
            .collect()
    }

    /// The cursor positions a touch emitted, in dispatch order.
    fn cursor_positions(output: TouchOutput) -> Vec<(f64, f64)> {
        output
            .events
            .into_iter()
            .flatten()
            .filter_map(|event| match event {
                WindowEvent::CursorMoved { position, .. } => Some((position.x, position.y)),
                _ => None,
            })
            .collect()
    }

    /// One finger step at scale 1.0; the tests scale distances off it.
    const STEP: f64 = DRAG_THRESHOLD_LOGICAL_PX + 1.0;

    // --- one finger ------------------------------------------------------

    #[test]
    fn a_quick_tap_clicks_the_left_button() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let down = mapper.translate(&touch(1, TouchPhase::Started, 40.0, 60.0), now, true, 1.0);
        assert!(!mapper.pan_active());
        assert!(down.pan.is_none());

        let click = names(mapper.translate(
            &touch(1, TouchPhase::Ended, 42.0, 60.0),
            now + Duration::from_millis(80),
            true,
            1.0,
        ));

        // The click lands where the finger lifted, so the cursor moves there
        // before the button edge.
        assert_eq!(
            click,
            vec![
                "move".to_owned(),
                "Pressed Left".to_owned(),
                "Released Left".to_owned()
            ]
        );
    }

    #[test]
    fn a_battlefield_drag_pans_the_camera_instead_of_drawing_a_box() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let down = mapper.translate(&touch(1, TouchPhase::Started, 40.0, 60.0), now, true, 1.0);
        assert_eq!(names(down), vec!["move".to_owned()]);

        // Crossing the threshold turns the drag into a camera pan: no button,
        // and the map follows the hand from where it landed, so the camera
        // moves against the finger.
        let first = mapper.translate(
            &touch(1, TouchPhase::Moved, 40.0 + STEP, 60.0),
            now,
            true,
            1.0,
        );
        assert_eq!(first.pan, Some((-(STEP as f32), 0.0)));
        assert_eq!(names(first), Vec::<String>::new());
        assert!(mapper.pan_active());

        // The next move only adds its own displacement.
        let second = mapper.translate(
            &touch(1, TouchPhase::Moved, 40.0 + STEP + 30.0, 60.0 + 20.0),
            now,
            true,
            1.0,
        );
        assert_eq!(second.pan, Some((-30.0, -20.0)));

        // Lifting ends the pan silently: no click, no box.
        let lift = mapper.translate(
            &touch(1, TouchPhase::Ended, 40.0 + STEP + 30.0, 60.0 + 20.0),
            now + Duration::from_millis(200),
            true,
            1.0,
        );
        assert!(lift.pan.is_none());
        assert!(!lift.deselect);
        assert!(names(lift).is_empty());
        assert!(!mapper.pan_active());
    }

    #[test]
    fn a_ui_drag_keeps_the_left_button_drag() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ = names(mapper.translate(
            &touch(1, TouchPhase::Started, 300.0, 400.0),
            now,
            false,
            1.0,
        ));

        let upgrade = mapper.translate(
            &touch(1, TouchPhase::Moved, 300.0 + STEP, 400.0),
            now,
            false,
            1.0,
        );

        // Over the sidebar the same left-button drag moves a slider or list,
        // so the press must land on the grabbed origin, not the moved finger,
        // and no camera motion is reported.
        assert!(upgrade.pan.is_none());
        assert_eq!(
            names(upgrade),
            vec!["move".to_owned(), "Pressed Left".to_owned()]
        );

        let follow = mapper.translate(
            &touch(1, TouchPhase::Moved, 300.0 + STEP + 25.0, 400.0),
            now,
            false,
            1.0,
        );
        assert_eq!(names(follow), vec!["move".to_owned()]);

        let lift = names(mapper.translate(
            &touch(1, TouchPhase::Ended, 300.0 + STEP + 25.0, 400.0),
            now + Duration::from_millis(200),
            false,
            1.0,
        ));
        assert_eq!(lift, vec!["move".to_owned(), "Released Left".to_owned()]);
    }

    #[test]
    fn the_drag_threshold_scales_with_display_density() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ = names(mapper.translate(&touch(1, TouchPhase::Started, 0.0, 0.0), now, true, 2.0));

        // At scale 2.0 the threshold is 30 physical px: 25 px stays pending,
        // 31 px becomes a pan.
        let below = mapper.translate(&touch(1, TouchPhase::Moved, 25.0, 0.0), now, true, 2.0);
        assert!(below.pan.is_none());
        assert_eq!(names(below), vec!["move".to_owned()]);

        let above = mapper.translate(&touch(1, TouchPhase::Moved, 31.0, 0.0), now, true, 2.0);
        assert_eq!(above.pan, Some((-31.0, 0.0)));
        assert!(names(above).is_empty());
    }

    #[test]
    fn menu_touches_keep_the_plain_click_gestures() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ = names(mapper.translate(
            &touch(1, TouchPhase::Started, 100.0, 100.0),
            now,
            false,
            1.0,
        ));

        // A second finger cannot start a battlefield box on a menu, so it is
        // ignored...
        let second = names(mapper.translate(
            &touch(2, TouchPhase::Started, 200.0, 200.0),
            now,
            false,
            1.0,
        ));

        // ...and the first finger still drags a slider.
        let drag = names(mapper.translate(
            &touch(1, TouchPhase::Moved, 100.0 + STEP, 100.0),
            now,
            false,
            1.0,
        ));

        assert!(second.is_empty());
        assert_eq!(drag, vec!["move".to_owned(), "Pressed Left".to_owned()]);
    }

    // --- long press ------------------------------------------------------

    #[test]
    fn a_long_still_touch_fires_the_right_button_immediately() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ = names(mapper.translate(&touch(1, TouchPhase::Started, 40.0, 60.0), now, true, 1.0));

        let fired = names(mapper.translate(
            &touch(1, TouchPhase::Moved, 42.0, 60.0),
            now + LONG_PRESS + Duration::from_millis(1),
            true,
            1.0,
        ));
        let consumed = names(mapper.translate(
            &touch(1, TouchPhase::Ended, 42.0, 60.0),
            now + LONG_PRESS + Duration::from_millis(40),
            true,
            1.0,
        ));

        assert_eq!(
            fired,
            vec![
                "move".to_owned(),
                "Pressed Right".to_owned(),
                "Released Right".to_owned()
            ]
        );
        assert!(consumed.is_empty());
    }

    #[test]
    fn a_long_press_without_any_move_fires_on_release() {
        // Coarse delivery can skip move events entirely; the release must
        // still read as a long press, not as a tap.
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ = names(mapper.translate(&touch(1, TouchPhase::Started, 40.0, 60.0), now, true, 1.0));

        let fired = names(mapper.translate(
            &touch(1, TouchPhase::Ended, 40.0, 60.0),
            now + LONG_PRESS + Duration::from_millis(50),
            true,
            1.0,
        ));

        assert_eq!(
            fired,
            vec![
                "move".to_owned(),
                "Pressed Right".to_owned(),
                "Released Right".to_owned()
            ]
        );
    }

    #[test]
    fn a_cancelled_pending_touch_clicks_nothing() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ = names(mapper.translate(&touch(1, TouchPhase::Started, 40.0, 60.0), now, true, 1.0));

        let cancelled = names(mapper.translate(
            &touch(1, TouchPhase::Cancelled, 42.0, 60.0),
            now + Duration::from_millis(80),
            true,
            1.0,
        ));

        assert!(cancelled.is_empty());
    }

    // --- two fingers -----------------------------------------------------

    #[test]
    fn two_fingers_box_the_area_between_them() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));

        // The second contact lands: the pair presses nothing yet; a press
        // lands on the landing rectangle's top-left once the pair moves.
        let takeover =
            mapper.translate(&touch(2, TouchPhase::Started, 200.0, 100.0), now, true, 1.0);
        assert!(takeover.pan.is_none());
        assert!(mapper.pan_active());
        assert_eq!(names(takeover), vec!["move".to_owned()]);

        // The second finger moves past the threshold: the box opens at the
        // landing corner and its moving corner sits on the pair's bottom-right,
        // so the box covers the fingers, not the centroid's travel.
        let started = mapper.translate(&touch(2, TouchPhase::Moved, 240.0, 100.0), now, true, 1.0);
        assert!(started.pan.is_none());
        assert_eq!(
            names(started),
            vec![
                "move".to_owned(),
                "Pressed Left".to_owned(),
                "move".to_owned()
            ]
        );

        // The moving corner keeps tracking the pair's bottom-right.
        let follow = mapper.translate(&touch(2, TouchPhase::Moved, 300.0, 160.0), now, true, 1.0);
        assert!(follow.pan.is_none());
        assert_eq!(names(follow), vec!["move".to_owned()]);

        // Lifting one finger releases the box at the pair's final
        // bottom-right; the pan flag clears.
        let lift = mapper.translate(
            &touch(2, TouchPhase::Ended, 300.0, 160.0),
            now + Duration::from_millis(40),
            true,
            1.0,
        );
        assert_eq!(
            names(lift),
            vec!["move".to_owned(), "Released Left".to_owned()]
        );
        assert!(!mapper.pan_active());

        // The survivor's own lift must not click: the gesture is over.
        let survivor = mapper.translate(
            &touch(1, TouchPhase::Ended, 100.0, 100.0),
            now + Duration::from_millis(60),
            true,
            1.0,
        );
        assert!(!survivor.deselect);
        assert!(names(survivor).is_empty());
    }

    #[test]
    fn the_box_covers_the_rectangle_between_the_two_contacts() {
        // Regression: the box used to run between the pair's centroids, so it
        // covered about half of the area the hand held. The box now spans the
        // pair's rectangle, and both edges follow the contacts: the press lands
        // on the landing top-left, then each move re-anchors that corner to the
        // pair's current top-left and moves the opposite corner with it.
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 200.0), now, true, 1.0));
        let _ =
            names(mapper.translate(&touch(2, TouchPhase::Started, 200.0, 100.0), now, true, 1.0));

        let opened = mapper.translate(
            &touch(2, TouchPhase::Moved, 200.0 + STEP, 100.0),
            now,
            true,
            1.0,
        );
        // Press at the landing rectangle's top-left, then the moving corner at
        // the pair's current bottom-right: (216, 200) covers both fingers.
        assert_eq!(opened.band_anchor, Some((100.0, 100.0)));
        assert_eq!(
            cursor_positions(opened),
            vec![(100.0, 100.0), (200.0 + STEP, 200.0)]
        );

        // Expanding the pair up and left re-anchors the box's fixed corner,
        // so the box can grow in every direction, not just right and down.
        let wider = mapper.translate(&touch(1, TouchPhase::Moved, 50.0, 50.0), now, true, 1.0);
        assert_eq!(wider.band_anchor, Some((50.0, 50.0)));
        assert_eq!(cursor_positions(wider), vec![(200.0 + STEP, 100.0)]);

        // The release commits that same rectangle.
        let released = mapper.translate(&touch(1, TouchPhase::Ended, 50.0, 50.0), now, true, 1.0);
        assert_eq!(released.band_anchor, Some((50.0, 50.0)));
        assert!(!released.deselect);
        assert_eq!(cursor_positions(released), vec![(200.0 + STEP, 100.0)]);
    }

    #[test]
    fn a_second_finger_over_the_sidebar_does_not_start_a_box() {
        // Only battlefield contacts drive the box: otherwise a pair straddling
        // the sidebar edge could put the box's press on a sidebar gadget.
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 700.0, 300.0), now, true, 1.0));

        let sidebar = names(mapper.translate(
            &touch(2, TouchPhase::Started, 900.0, 300.0),
            now,
            false,
            1.0,
        ));
        assert!(sidebar.is_empty());
        assert!(!mapper.pan_active());

        // The first finger is alone, so its drag pans instead of boxing.
        let drag = mapper.translate(
            &touch(1, TouchPhase::Moved, 700.0 + STEP, 300.0),
            now,
            true,
            1.0,
        );
        assert_eq!(drag.pan, Some((-(STEP as f32), 0.0)));
    }

    #[test]
    fn a_two_finger_tap_cancels_the_selection() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));
        let _ =
            names(mapper.translate(&touch(2, TouchPhase::Started, 200.0, 200.0), now, true, 1.0));

        // The first of the tap's two contacts lifts: no answer yet.
        let first = mapper.translate(
            &touch(2, TouchPhase::Ended, 202.0, 200.0),
            now + Duration::from_millis(60),
            true,
            1.0,
        );
        assert!(!first.deselect);
        assert!(names(first).is_empty());
        // The survivor moving does not turn the tap into a box: there is no
        // pair left to span, so the gesture stays the tap it started as.
        let survivor_move = mapper.translate(
            &touch(1, TouchPhase::Moved, 100.0 + STEP, 100.0),
            now + Duration::from_millis(70),
            true,
            1.0,
        );
        assert!(survivor_move.pan.is_none());
        assert!(!survivor_move.deselect);
        assert!(names(survivor_move).is_empty());

        // The surviving contact lifts: the tap resolves to the selection
        // clear, and no left click is synthesized for either contact.
        let second = mapper.translate(
            &touch(1, TouchPhase::Ended, 100.0, 100.0),
            now + Duration::from_millis(80),
            true,
            1.0,
        );
        assert!(second.deselect);
        assert!(names(second).is_empty());
        assert!(!mapper.pan_active());
    }

    #[test]
    fn a_two_finger_tap_that_moves_becomes_a_box() {
        // The same two contacts, but the pair moved past the threshold: the
        // lift releases the box instead of clearing the selection.
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));
        let _ =
            names(mapper.translate(&touch(2, TouchPhase::Started, 200.0, 100.0), now, true, 1.0));
        let _ = names(mapper.translate(&touch(2, TouchPhase::Moved, 300.0, 100.0), now, true, 1.0));

        let lift = mapper.translate(
            &touch(1, TouchPhase::Ended, 100.0, 100.0),
            now + Duration::from_millis(40),
            true,
            1.0,
        );
        assert!(!lift.deselect);
        assert_eq!(
            names(lift),
            vec!["move".to_owned(), "Released Left".to_owned()]
        );
    }

    #[test]
    fn a_second_finger_after_a_pan_starts_the_box() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));
        let first_pan = mapper.translate(
            &touch(1, TouchPhase::Moved, 100.0 + STEP, 100.0),
            now,
            true,
            1.0,
        );
        assert!(first_pan.pan.is_some());

        // The second finger takes over as the select-box gesture; the pair
        // anchors where both fingers are now and drops further camera motion.
        let takeover =
            mapper.translate(&touch(2, TouchPhase::Started, 200.0, 100.0), now, true, 1.0);
        assert!(takeover.pan.is_none());
        assert_eq!(names(takeover), vec!["move".to_owned()]);

        let moved = mapper.translate(&touch(2, TouchPhase::Moved, 240.0, 100.0), now, true, 1.0);
        assert!(moved.pan.is_none());
        assert_eq!(
            names(moved),
            vec![
                "move".to_owned(),
                "Pressed Left".to_owned(),
                "move".to_owned()
            ]
        );
    }

    #[test]
    fn lifting_a_pan_finger_never_clicks() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));
        let _ = names(mapper.translate(
            &touch(1, TouchPhase::Moved, 100.0 + STEP, 100.0),
            now,
            true,
            1.0,
        ));

        // Lifting a panning finger ends the pan without a click, even when the
        // hold was long enough for the long-press fallback.
        let lift = mapper.translate(
            &touch(1, TouchPhase::Ended, 100.0 + STEP, 100.0),
            now + LONG_PRESS + Duration::from_millis(10),
            true,
            1.0,
        );
        assert!(!lift.deselect);
        assert!(names(lift).is_empty());
    }

    #[test]
    fn a_third_finger_is_ignored() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));
        let _ =
            names(mapper.translate(&touch(2, TouchPhase::Started, 200.0, 100.0), now, true, 1.0));

        let third =
            names(mapper.translate(&touch(3, TouchPhase::Started, 300.0, 100.0), now, true, 1.0));
        assert!(third.is_empty());

        // The box keeps following the two fingers it started with: moving
        // finger 2 right by 40 px puts the box's moving corner there.
        let moved = mapper.translate(&touch(2, TouchPhase::Moved, 240.0, 100.0), now, true, 1.0);
        assert_eq!(
            names(moved),
            vec![
                "move".to_owned(),
                "Pressed Left".to_owned(),
                "move".to_owned()
            ]
        );
    }

    #[test]
    fn a_new_finger_after_a_lift_keeps_the_live_hand() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));
        let _ =
            names(mapper.translate(&touch(2, TouchPhase::Started, 200.0, 100.0), now, true, 1.0));
        let _ = names(mapper.translate(
            &touch(1, TouchPhase::Ended, 100.0, 100.0),
            now + Duration::from_millis(40),
            true,
            1.0,
        ));

        // The lifted finger left a hole in the table: the next touch must take
        // that slot instead of overwriting finger 2, which is still down. The
        // pair is back, so the box opens from the landing anchor and follows
        // the new pair's bottom-right.
        let _ =
            names(mapper.translate(&touch(3, TouchPhase::Started, 300.0, 100.0), now, true, 1.0));
        let moved = mapper.translate(&touch(2, TouchPhase::Moved, 240.0, 100.0), now, true, 1.0);
        assert_eq!(
            names(moved),
            vec![
                "move".to_owned(),
                "Pressed Left".to_owned(),
                "move".to_owned()
            ]
        );
    }

    #[test]
    fn one_contact_delivered_twice_is_still_one_finger() {
        // winit reissues a Windows touch from `WM_TOUCH` and `WM_POINTER*`
        // under two ids. The copy must not read as a second finger, or the
        // first tap of every gesture becomes a select box.
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let first =
            names(mapper.translate(&touch(7, TouchPhase::Started, 120.0, 90.0), now, true, 1.0));
        let copy =
            names(mapper.translate(&touch(8, TouchPhase::Started, 120.5, 90.5), now, true, 1.0));

        assert_eq!(first, vec!["move".to_owned()]);
        assert!(copy.is_empty());
        assert!(!mapper.pan_active());

        // The surviving finger still pans normally.
        let pan = mapper.translate(
            &touch(7, TouchPhase::Moved, 120.0 + STEP, 90.0),
            now,
            true,
            1.0,
        );
        assert_eq!(pan.pan, Some((-(STEP as f32), 0.0)));
    }

    // --- reset and the edge-scroll gate ----------------------------------

    #[test]
    fn a_reset_drops_a_lost_touch_stream() {
        let mut mapper = TouchMouseMapper::new();
        let now = Instant::now();
        let _ =
            names(mapper.translate(&touch(1, TouchPhase::Started, 100.0, 100.0), now, true, 1.0));
        let _ =
            names(mapper.translate(&touch(2, TouchPhase::Started, 200.0, 100.0), now, true, 1.0));

        // A cancelled contact or a focus loss can end a stream without an
        // `Ended`; the phantom pair must not turn the next tap into a box.
        mapper.reset();

        let down =
            names(mapper.translate(&touch(5, TouchPhase::Started, 40.0, 60.0), now, true, 1.0));
        let click = names(mapper.translate(
            &touch(5, TouchPhase::Ended, 40.0, 60.0),
            now + Duration::from_millis(60),
            true,
            1.0,
        ));

        assert_eq!(down, vec!["move".to_owned()]);
        assert_eq!(
            click,
            vec![
                "move".to_owned(),
                "Pressed Left".to_owned(),
                "Released Left".to_owned()
            ]
        );
    }

    #[test]
    fn a_touch_pan_removes_the_edge_scroll_band() {
        use crate::app::input::camera::touch_owns_pointer;
        use crate::app::input::state::MatchInputState;

        let mut input = MatchInputState::new(Default::default());
        let now = Instant::now();
        let _ = input.touch_mapper.translate(
            &touch(1, TouchPhase::Started, 100.0, 100.0),
            now,
            true,
            1.0,
        );
        input.cursor_from_touch = true;
        assert!(touch_owns_pointer(&input));

        // A one-finger battlefield drag is a camera pan; it must not let the
        // parked synthetic cursor keep the edge band engaged.
        let _ = input.touch_mapper.translate(
            &touch(1, TouchPhase::Moved, 100.0 + STEP, 100.0),
            now,
            true,
            1.0,
        );
        assert!(input.touch_mapper.pan_active());
        assert!(touch_owns_pointer(&input));
    }

    #[test]
    fn a_parked_touch_pointer_removes_the_edge_scroll_band() {
        use crate::app::input::camera::touch_owns_pointer;
        use crate::app::input::state::MatchInputState;

        // Without a real pointer the synthetic cursor stays where the last
        // contact left it, so the band would scroll with nothing to carry it
        // away. The handler raises this on every touch and the next hardware
        // `CursorMoved` drops it again.
        let mut input = MatchInputState::new(Default::default());
        assert!(!touch_owns_pointer(&input));

        input.cursor_from_touch = true;
        assert!(touch_owns_pointer(&input));

        input.cursor_from_touch = false;
        assert!(!touch_owns_pointer(&input));
    }
}
