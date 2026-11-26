//! The SimpleGUI face (`lib/ashes/simple_gui.cpp` + `simple_gui_component.cpp`),
//! ported for BW-11-05-01 (acceptance: callback order and parameters match).
//!
//! `SimpleGUI` keeps the root `components_` vector plus four focus lists
//! (`mouseButtonFocusList_`, `crossFocusList_`, `dragFocusList_`,
//! `dropFocusList_`). Key events route through the mouse-button focus list
//! (`handleKeyEvent`, simple_gui.cpp:818), then click tracking
//! (`processClickKey`, :1058), then drag tracking (`processDragKey`, :1088),
//! and only when nothing handled them fall back to the roots LAST-to-FIRST.
//! Mouse events try the drag-move first (`processDragMove`, :1186) then the
//! plain move (`processMouseMove`, :1168). Enter/leave detection
//! (`detectEnterLeaveEvents`, :953) fires leave-before-enter on the OLD then
//! NEW over-component, using the mouse-cursor position and a zeroed event.
//!
//! The Python script object is modeled as [`ScriptObject`]: every callback
//! invocation appends a [`ScriptCall`] to a shared log (component name,
//! callback name, argument tuple), and the script's truthiness — pinned per
//! callback through `set_handled`, defaulting to false — is the `handled`
//! result (`invokeMouseEventHandler`/`invokeKeyEventHandler`,
//! simple_gui_component.cpp:4310/4360: `bool handled = false;` then
//! `Script::setAnswer` overwrites it from the return value).
//!
//! The locked argument tuples (`invokeMouseEventHandler` receives `mousePos`
//! but never puts it in the tuple — the docstrings claiming a position tuple
//! are stale; `invokeKeyEventHandler` appends the event, optional dragged):
//!
//! | callback                 | script arguments            | gate                     |
//! |--------------------------|-----------------------------|--------------------------|
//! | `handleKeyEvent`         | `(event)`                   | `focus()`                |
//! | `handleMouseButtonEvent` | `(comp, event)`             | `mouseButtonFocus()`     |
//! | `handleMouseEvent`       | `(comp, event)`             | `moveFocus()` + hit      |
//! | `handleAxisEvent`        | `(event)`                   | `focus()`                |
//! | `handleMouseClickEvent`  | `(comp)`                    | click tracking           |
//! | `handleMouseEnterEvent`  | `(comp)`                    | cross focus detection    |
//! | `handleMouseLeaveEvent`  | `(comp)`                    | cross focus detection    |
//! | `handleDragStartEvent`   | `(comp)`                    | drag focus + distance    |
//! | `handleDragStopEvent`    | `(comp)`                    | drag stop                |
//! | `handleDropEvent`        | `(comp, dragged)`           | `dropFocus()` + accepts  |
//! | `handleDragEnterEvent`   | `(comp, dragged)`           | drop focus; return sets  |
//! |                          |                             | `targetAccepts`          |
//! | `handleDragLeaveEvent`   | `(comp, dragged)`           | drop focus detection     |
//!
//! Setting a focus flag (`IMPLEMENT_FOCUS_FUNCTION`,
//! simple_gui_component.cpp:3731) is a no-op when unchanged, notifies the
//! script attribute with a single int, then adds/removes the component on the
//! matching focus list — with side effects on pending click/drag state
//! (`delMouseButtonFocus`/`delMouseDragFocus`/`delMouseDropFocus`,
//! simple_gui.cpp:1252/1408/1428). `focus`/`moveFocus` have EMPTY list
//! functions ("now handled through the hierarchy").
//!
//! Deferred faces (out of BW-11-05-01's callback scope, documented in the
//! todo lane): the render/draw path with `updateGUI_`, `appFocus` and the
//! resolution tracking; the anchor/layout engine (`layout`,
//! `nearestRelativeDimensions`) — a component's hit quad is its `position`
//! and `size` rectangle through an identity run-time transform; the animated
//! `MouseCursor` (only `isActive`/`position` participate in routing); the
//! D3D scissor conversion inside `commitClipRegion` (the port keeps the
//! clip-space arithmetic and the empty-region failure).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Sub;
use std::rc::{Rc, Weak};

/// `KeyCode::KEY_LEFTMOUSE` (key_code.hpp:184).
pub const KEY_LEFTMOUSE: u16 = 0x100;
/// `KeyCode::KEY_RIGHTMOUSE` (key_code.hpp:187).
pub const KEY_RIGHTMOUSE: u16 = 0x101;
/// `KeyCode::KEY_MIDDLEMOUSE` (key_code.hpp:190).
pub const KEY_MIDDLEMOUSE: u16 = 0x102;

/// `c_defaultDrgaDistance` (simple_gui.cpp:41 — the reference's typo kept).
pub const DEFAULT_DRAG_DISTANCE: f32 = 0.002;

/// A 2D point in GUI clip coordinates: x grows right, y grows DOWN
/// (`clipBounds` walks `botLeft.y -= clipHeight`, so the top edge carries the
/// larger y).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector2 {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

impl Vector2 {
    /// `(dragStartPos - event.cursorPosition()).lengthSquared()`.
    fn length_squared(self) -> f32 {
        self.x * self.x + self.y * self.y
    }
}

impl Sub for Vector2 {
    type Output = Vector2;
    fn sub(self, rhs: Vector2) -> Vector2 {
        Vector2 {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

/// A 3D position: x/y are the top-left corner of the component quad in clip
/// coordinates, z is the depth `reSort` orders roots by (`depth_compare_gui_
/// components`, simple_gui.cpp:115 — higher z first).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector3 {
    /// Horizontal coordinate of the top-left corner.
    pub x: f32,
    /// Vertical coordinate of the top-left corner (larger y = higher up).
    pub y: f32,
    /// Depth; roots with larger z sort first and draw first.
    pub z: f32,
}

/// The key event (`KeyEvent`, lib/input/input.hpp): `state` is the repeat
/// count — `>= 0` is a key-down, `-1` a key-up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyEvent {
    /// `KeyCode` value; mouse buttons are 0x100..=0x102.
    pub key: u16,
    /// Repeat count; negative means key-up (`isKeyDown` = `state_ >= 0`).
    pub state: i32,
    /// Modifier bitset.
    pub modifiers: u32,
    /// Cursor position at the event.
    pub cursor_position: Vector2,
}

impl KeyEvent {
    /// `KeyEvent::isKeyDown`.
    pub fn is_key_down(&self) -> bool {
        self.state >= 0
    }
    /// `KeyEvent::isKeyUp`.
    pub fn is_key_up(&self) -> bool {
        self.state < 0
    }
    /// `KeyEvent::isMouseButton` (input.ipp:131): exactly the three mouse
    /// button keys.
    pub fn is_mouse_button(&self) -> bool {
        self.key == KEY_LEFTMOUSE || self.key == KEY_RIGHTMOUSE || self.key == KEY_MIDDLEMOUSE
    }
}

/// The mouse event (`MouseEvent`): deltas plus the cursor position. Enter/
/// leave detection fills it with `event.fill(0, 0, 0, mousePos)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseEvent {
    /// Horizontal delta.
    pub dx: i32,
    /// Vertical delta.
    pub dy: i32,
    /// Wheel delta.
    pub dz: i32,
    /// Cursor position at the event.
    pub cursor_position: Vector2,
}

impl MouseEvent {
    /// `MouseEvent::fill`.
    pub fn fill(dx: i32, dy: i32, dz: i32, cursor_position: Vector2) -> MouseEvent {
        MouseEvent {
            dx,
            dy,
            dz,
            cursor_position,
        }
    }
}

/// The axis event (`AxisEvent`): gamepad stick input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisEvent {
    /// Axis identifier.
    pub axis: i32,
    /// Stick position, -1..1.
    pub value: f32,
    /// Time since the axis was last processed.
    pub dtime: f32,
}

/// One element of a recorded callback argument tuple.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptArg {
    /// A component reference (`this` or the dragged component).
    Component(String),
    /// A key event (`Script::getData(event)`).
    Key(KeyEvent),
    /// A mouse event.
    Mouse(MouseEvent),
    /// An axis event.
    Axis(AxisEvent),
    /// An int (`Py_BuildValue("(i)", intState)` in the focus setters).
    Int(i32),
}

/// One recorded script invocation: component, callback name, argument tuple.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptCall {
    /// The script object's name (the component it is bound to, usually).
    pub component: String,
    /// The attribute invoked on the script object.
    pub callback: String,
    /// The argument tuple in order.
    pub args: Vec<ScriptArg>,
}

/// The shared callback log every [`ScriptObject`] appends to.
pub type ScriptLog = Rc<RefCell<Vec<ScriptCall>>>;

/// The stand-in for a component's Python script object.
///
/// Every invocation appends a [`ScriptCall`] to the shared log and returns
/// the pinned truthiness for that callback (`Script::setAnswer(ret, handled,
/// ...)`: a missing/false return leaves `handled = false`).
#[derive(Clone)]
pub struct ScriptObject {
    name: Rc<str>,
    log: ScriptLog,
    handled: Rc<RefCell<HashMap<&'static str, bool>>>,
}

impl ScriptObject {
    /// Bind a script object to the shared log.
    pub fn new(name: impl AsRef<str>, log: &ScriptLog) -> ScriptObject {
        ScriptObject {
            name: Rc::from(name.as_ref()),
            log: Rc::clone(log),
            handled: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// The script object's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Pin the truthiness one callback returns (`Script::setAnswer`).
    pub fn set_handled(&self, callback: &'static str, value: bool) {
        self.handled.borrow_mut().insert(callback, value);
    }

    /// `Script::ask` + `Script::setAnswer`: record the call, answer with the
    /// pinned truthiness (default false).
    fn invoke(&self, callback: &'static str, args: Vec<ScriptArg>) -> bool {
        self.log.borrow_mut().push(ScriptCall {
            component: self.name.to_string(),
            callback: callback.to_string(),
            args,
        });
        self.handled
            .borrow()
            .get(callback)
            .copied()
            .unwrap_or(false)
    }

    /// `Script::call` (the focus-flag setter notification): record the call,
    /// ignore any return.
    fn notify(&self, callback: &'static str, state: bool) {
        self.log.borrow_mut().push(ScriptCall {
            component: self.name.to_string(),
            callback: callback.to_string(),
            args: vec![ScriptArg::Int(state as i32)],
        });
    }
}

/// A clip region in GUI clip coordinates (`Vector4` in `pushClipRegion`):
/// `left <= x <= right`, and y grows DOWN so the top edge carries the larger
/// y (`isPointInClipRegion`: `pt.x >= r.x && pt.y <= r.y && pt.x <= r.z &&
/// pt.y >= r.w`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipRect {
    /// Left edge.
    pub left: f32,
    /// Top edge (the larger y).
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge (the smaller y).
    pub bottom: f32,
}

impl ClipRect {
    /// A region covering everything (a component with no `runTimeClipRegion_`
    /// restriction, in this port's model).
    fn unbounded() -> ClipRect {
        ClipRect {
            left: f32::MIN,
            top: f32::MAX,
            right: f32::MAX,
            bottom: f32::MIN,
        }
    }

    /// `pushClipRegion(Vector4)`'s intersection: `x = max, y = min,
    /// z = min, w = max` against the region below.
    fn intersect(self, other: ClipRect) -> ClipRect {
        ClipRect {
            left: self.left.max(other.left),
            top: self.top.min(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    /// `commitClipRegion`'s validity: the region is drawable only when both
    /// spans are positive; `hitTest` returns false when the push fails.
    fn is_empty(self) -> bool {
        self.right <= self.left || self.top <= self.bottom
    }

    /// `isPointInClipRegion` (simple_gui.cpp): inclusive on every edge, with
    /// the y inversion.
    fn contains(self, pt: Vector2) -> bool {
        pt.x >= self.left && pt.y <= self.top && pt.x <= self.right && pt.y >= self.bottom
    }
}

/// A GUI component handle.
pub type ComponentRef = Rc<RefCell<SimpleGuiComponent>>;

/// A `SimpleGUIComponent` (`simple_gui_component.hpp`): hierarchy, focus
/// flags, script object and the hit quad.
///
/// `children_` is an `OrderedStringMap` — a vector of key/value pairs kept in
/// the order entries were ADDED (stringmap.hpp:463) — so child callbacks fire
/// in insertion order, not key order.
pub struct SimpleGuiComponent {
    name: String,
    script: Option<ScriptObject>,
    /// Top-left corner and depth of the hit quad (identity run-time transform;
    /// the layout/anchor engine is a deferred face).
    position: Vector3,
    /// Quad size: width rightward, height downward (`botLeft.y -= height`).
    size: (f32, f32),
    visible: bool,
    allow_delete: bool,
    focus_: bool,
    mouse_button_focus_: bool,
    move_focus_: bool,
    cross_focus_: bool,
    drag_focus_: bool,
    drop_focus_: bool,
    /// `runTimeClipRegion_`: an extra clip the hit test intersects with.
    clip_region: Option<ClipRect>,
    children: Vec<(String, ComponentRef)>,
    parent: Option<Weak<RefCell<SimpleGuiComponent>>>,
    draw_order: u32,
    tree_cookie: u32,
}

impl SimpleGuiComponent {
    /// A visible, deletable component with no focus and no script.
    pub fn new(name: impl Into<String>) -> ComponentRef {
        Rc::new(RefCell::new(SimpleGuiComponent {
            name: name.into(),
            script: None,
            position: Vector3::default(),
            size: (0.0, 0.0),
            visible: true,
            allow_delete: true,
            focus_: false,
            mouse_button_focus_: false,
            move_focus_: false,
            cross_focus_: false,
            drag_focus_: false,
            drop_focus_: false,
            clip_region: None,
            children: Vec::new(),
            parent: None,
            draw_order: 0,
            tree_cookie: 0,
        }))
    }

    /// The component's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Bind the script object (`pScriptObject_`).
    pub fn set_script(&mut self, script: ScriptObject) {
        self.script = Some(script);
    }

    /// The bound script object, if any.
    pub fn script(&self) -> Option<&ScriptObject> {
        self.script.as_ref()
    }

    /// Move the component (top-left corner + depth).
    pub fn set_position(&mut self, position: Vector3) {
        self.position = position;
    }

    /// Size the hit quad.
    pub fn set_size(&mut self, width: f32, height: f32) {
        self.size = (width, height);
    }

    /// Show/hide (`visible_`; an invisible component cannot handle input,
    /// and neither can the children below it).
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// `allowDelete()`: `removeSimpleComponent` only removes components that
    /// allow it.
    pub fn set_allow_delete(&mut self, allow_delete: bool) {
        self.allow_delete = allow_delete;
    }

    /// `allowDelete()`.
    pub fn allow_delete(&self) -> bool {
        self.allow_delete
    }

    /// Set `runTimeClipRegion_` (None = unbounded).
    pub fn set_clip_region(&mut self, clip_region: Option<ClipRect>) {
        self.clip_region = clip_region;
    }

    /// The last assigned draw order (`calcDrawOrderRecursively`).
    pub fn draw_order(&self) -> u32 {
        self.draw_order
    }

    /// `focus()` flag.
    pub fn focus(&self) -> bool {
        self.focus_
    }

    /// `mouseButtonFocus()` flag.
    pub fn mouse_button_focus(&self) -> bool {
        self.mouse_button_focus_
    }

    /// `moveFocus()` flag.
    pub fn move_focus(&self) -> bool {
        self.move_focus_
    }

    /// `crossFocus()` flag.
    pub fn cross_focus(&self) -> bool {
        self.cross_focus_
    }

    /// `dragFocus()` flag.
    pub fn drag_focus(&self) -> bool {
        self.drag_focus_
    }

    /// `dropFocus()` flag.
    pub fn drop_focus(&self) -> bool {
        self.drop_focus_
    }

    /// Fetch a direct child by key.
    pub fn child(&self, name: &str) -> Option<ComponentRef> {
        self.children
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, c)| Rc::clone(c))
    }

    /// `handlesInput()` (simple_gui_component.ipp:489): visible, and every
    /// ancestor visible.
    pub fn handles_input(&self) -> bool {
        if !self.visible {
            return false;
        }
        match &self.parent {
            Some(parent) => parent
                .upgrade()
                .is_none_or(|parent| parent.borrow().handles_input()),
            None => true,
        }
    }

    /// `hitTest` (simple_gui_component.cpp:4444): gate on `handlesInput`,
    /// project the quad (identity run-time transform here) and run the
    /// reference's sign test — the point must sit on the same side of all
    /// four edges — then require the position inside the effective clip
    /// region (own `runTimeClipRegion_` intersected with the manager's
    /// current clip stack top).
    pub fn hit_test(&self, clip_stack_top: Option<ClipRect>, test_pos: Vector2) -> bool {
        if !self.handles_input() {
            return false;
        }

        // clipBounds: topLeft, topRight, botRight, botLeft around position/size.
        let (width, height) = self.size;
        let (x, y) = (self.position.x, self.position.y);
        let z = self.position.z;
        let corners = [
            (x, y, z),                  // p0 top-left
            (x + width, y, z),          // p1 top-right
            (x + width, y - height, z), // p2 bottom-right
            (x, y - height, z),         // p3 bottom-left
        ];

        let cross_z = |ax: f32, ay: f32, bx: f32, by: f32| ax * by - ay * bx;
        let sig1 = cross_z(
            test_pos.x - corners[3].0,
            test_pos.y - corners[3].1,
            corners[0].0 - corners[3].0,
            corners[0].1 - corners[3].1,
        );
        for i in 0..3 {
            let sig2 = cross_z(
                test_pos.x - corners[i].0,
                test_pos.y - corners[i].1,
                corners[i + 1].0 - corners[i].0,
                corners[i + 1].1 - corners[i].1,
            );
            if sig1 * sig2 <= 0.0 {
                return false;
            }
        }

        // pushClipRegion(runTimeClipRegion_) + isPointInClipRegion: an empty
        // intersection fails the push and the hit; otherwise the point must
        // sit inside the pushed region.
        let own = self.clip_region.unwrap_or_else(ClipRect::unbounded);
        let region = match clip_stack_top {
            Some(below) => own.intersect(below),
            None => own,
        };
        !region.is_empty() && region.contains(test_pos)
    }

    /// `SimpleGUIComponent::handleKeyEvent` (:3848): children first (in
    /// insertion order, stop on handled), then — only when `focus()` — the
    /// script with `(event)` alone.
    pub fn handle_key_event(&self, event: KeyEvent) -> bool {
        if !self.handles_input() {
            return false;
        }

        let mut handled = false;
        for (_, child) in &self.children {
            handled = child.borrow().handle_key_event(event);
            if handled {
                break;
            }
        }

        if let Some(script) = &self.script
            && !handled
            && self.focus()
        {
            handled = script.invoke("handleKeyEvent", vec![ScriptArg::Key(event)]);
        }

        handled
    }

    /// `SimpleGUIComponent::handleMouseButtonEvent` (:3905): only the
    /// `mouseButtonFocus()` gate — no children, no hit test — then the script
    /// with `(comp, event)`.
    pub fn handle_mouse_button_event(&self, event: KeyEvent) -> bool {
        if !self.mouse_button_focus() {
            return false;
        }
        match &self.script {
            Some(script) => script.invoke(
                "handleMouseButtonEvent",
                vec![
                    ScriptArg::Component(self.name.clone()),
                    ScriptArg::Key(event),
                ],
            ),
            None => false,
        }
    }

    /// `SimpleGUIComponent::handleMouseEvent` (:3945): children whose quad
    /// the event position hits go first, then — when `moveFocus()` and the
    /// position hits the component itself — the script with `(comp, event)`.
    pub fn handle_mouse_event(&self, clip_stack_top: Option<ClipRect>, event: MouseEvent) -> bool {
        if !self.handles_input() {
            return false;
        }

        let mut handled = false;
        for (_, child) in &self.children {
            if child
                .borrow()
                .hit_test(clip_stack_top, event.cursor_position)
            {
                handled = child.borrow().handle_mouse_event(clip_stack_top, event);
                if handled {
                    break;
                }
            }
        }

        if let Some(script) = &self.script
            && self.move_focus()
            && !handled
            && self.hit_test(clip_stack_top, event.cursor_position)
        {
            handled = script.invoke(
                "handleMouseEvent",
                vec![
                    ScriptArg::Component(self.name.clone()),
                    ScriptArg::Mouse(event),
                ],
            );
        }

        handled
    }

    /// `SimpleGUIComponent::handleAxisEvent` (:4058): children first, then —
    /// only when `focus()` — the script with `(event)`.
    pub fn handle_axis_event(&self, event: AxisEvent) -> bool {
        if !self.handles_input() {
            return false;
        }

        let mut handled = false;
        for (_, child) in &self.children {
            handled = child.borrow().handle_axis_event(event);
            if handled {
                break;
            }
        }

        if let Some(script) = &self.script
            && !handled
            && self.focus()
        {
            handled = script.invoke("handleAxisEvent", vec![ScriptArg::Axis(event)]);
        }

        handled
    }

    /// `handleMouseEnterEvent` (:4081): the script with `(comp)` only — the
    /// position is passed to the invoker but never into the tuple.
    pub fn handle_mouse_enter_event(&self, _event: MouseEvent) -> bool {
        self.invoke_mouse_handler("handleMouseEnterEvent", None)
    }

    /// `handleMouseLeaveEvent` (:4103): the script with `(comp)`.
    pub fn handle_mouse_leave_event(&self, _event: MouseEvent) -> bool {
        self.invoke_mouse_handler("handleMouseLeaveEvent", None)
    }

    /// `handleMouseClickEvent` (:4137): the script with `(comp)`.
    pub fn handle_mouse_click_event(&self, _event: KeyEvent) -> bool {
        self.invoke_mouse_handler("handleMouseClickEvent", None)
    }

    /// `handleDragStartEvent` (:4181): the script with `(comp)`; the return
    /// says whether the component is willing to be dragged.
    pub fn handle_drag_start_event(&self, _event: KeyEvent) -> bool {
        self.invoke_mouse_handler("handleDragStartEvent", None)
    }

    /// `handleDragStopEvent` (:4211): the script with `(comp)`.
    pub fn handle_drag_stop_event(&self, _event: KeyEvent) -> bool {
        self.invoke_mouse_handler("handleDragStopEvent", None)
    }

    /// `handleDropEvent` (:4247): the script with `(comp, dragged)`.
    pub fn handle_drop_event(&self, dragged: &ComponentRef, _event: KeyEvent) -> bool {
        self.invoke_mouse_handler("handleDropEvent", Some(dragged))
    }

    /// `handleDragEnterEvent` (:4282): the script with `(comp, dragged)`; the
    /// return decides `targetAccepts` — a declined enter suppresses the drop.
    pub fn handle_drag_enter_event(&self, dragged: &ComponentRef, _event: MouseEvent) -> bool {
        self.invoke_mouse_handler("handleDragEnterEvent", Some(dragged))
    }

    /// `handleDragLeaveEvent` (:4312): the script with `(comp, dragged)`.
    pub fn handle_drag_leave_event(&self, dragged: &ComponentRef, _event: MouseEvent) -> bool {
        self.invoke_mouse_handler("handleDragLeaveEvent", Some(dragged))
    }

    /// `invokeMouseEventHandler` (:4331): `(comp)` or `(comp, dragged)`, the
    /// return's truthiness is the answer (default false).
    fn invoke_mouse_handler(&self, callback: &'static str, dragged: Option<&ComponentRef>) -> bool {
        match &self.script {
            Some(script) => {
                let mut args = vec![ScriptArg::Component(self.name.clone())];
                if let Some(dragged) = dragged {
                    args.push(ScriptArg::Component(dragged.borrow().name.clone()));
                }
                script.invoke(callback, args)
            }
            None => false,
        }
    }
}

/// Which focus flag a setter drives (`IMPLEMENT_FOCUS_FUNCTION` instances).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FocusKind {
    /// `focus_` / `addInputFocus` — EMPTY list functions.
    Focus,
    /// `mouseButtonFocus_` / `mouseButtonFocusList_`.
    MouseButton,
    /// `moveFocus_` / `addMouseMoveFocus` — EMPTY list functions.
    Move,
    /// `crossFocus_` / `crossFocusList_`.
    Cross,
    /// `dragFocus_` / `dragFocusList_`.
    Drag,
    /// `dropFocus_` / `dropFocusList_`.
    Drop,
}

/// Read the flag (`IMPLEMENT_FOCUS_FUNCTION`'s `ATTRIB`).
fn focus_flag(comp: &SimpleGuiComponent, which: FocusKind) -> bool {
    match which {
        FocusKind::Focus => comp.focus_,
        FocusKind::MouseButton => comp.mouse_button_focus_,
        FocusKind::Move => comp.move_focus_,
        FocusKind::Cross => comp.cross_focus_,
        FocusKind::Drag => comp.drag_focus_,
        FocusKind::Drop => comp.drop_focus_,
    }
}

/// Write the flag.
fn set_focus_flag(comp: &mut SimpleGuiComponent, which: FocusKind, state: bool) {
    match which {
        FocusKind::Focus => comp.focus_ = state,
        FocusKind::MouseButton => comp.mouse_button_focus_ = state,
        FocusKind::Move => comp.move_focus_ = state,
        FocusKind::Cross => comp.cross_focus_ = state,
        FocusKind::Drag => comp.drag_focus_ = state,
        FocusKind::Drop => comp.drop_focus_ = state,
    }
}

/// The script attribute the setter notifies (`#PY_NAME`).
fn focus_callback(which: FocusKind) -> &'static str {
    match which {
        FocusKind::Focus => "focus",
        FocusKind::MouseButton => "mouseButtonFocus",
        FocusKind::Move => "moveFocus",
        FocusKind::Cross => "crossFocus",
        FocusKind::Drag => "dragFocus",
        FocusKind::Drop => "dropFocus",
    }
}

/// The drop-tracking record (`DragInfo`): the button-down key event that
/// started the gesture, the dragged component, the drop target under the
/// cursor and whether that target accepted the drag.
#[derive(Clone)]
pub struct DragInfo {
    /// The button-down event (`dragInfo_->keyEvent`): `processDragMove` and
    /// `handleDragStartEvent` see THIS event, not the moves.
    pub key_event: KeyEvent,
    /// The dragged component.
    pub component: ComponentRef,
    /// The detected drop target (`tickDragFocusEvents`).
    pub target: Option<ComponentRef>,
    /// Whether the drag crossed `dragDistanceSqr_`.
    pub dragging: bool,
    /// Whether the target's `handleDragEnterEvent` returned true.
    pub target_accepts: bool,
}

/// The mouse cursor face routing consults (`MouseCursor::isActive`/
/// `position`); the animated cursor itself is a deferred face.
#[derive(Clone, Copy, Debug)]
pub struct MouseCursor {
    /// Cursor position in clip coordinates (used ONLY by enter/leave
    /// detection; the mouse-event routing uses the event's own position).
    pub position: Vector2,
    /// `isActive()`: enter events fire only for an active cursor; an
    /// inactive one forces a leave.
    pub active: bool,
}

/// The `SimpleGUI` singleton, ported (`lib/ashes/simple_gui.cpp`).
pub struct SimpleGui {
    components: Vec<ComponentRef>,
    mouse_button_focus_list: Vec<ComponentRef>,
    cross_focus_list: Vec<ComponentRef>,
    drag_focus_list: Vec<ComponentRef>,
    drop_focus_list: Vec<ComponentRef>,
    click_component: Option<ComponentRef>,
    drag_info: Option<DragInfo>,
    current_cross_focus: Option<ComponentRef>,
    current_drop_focus: Option<ComponentRef>,
    drag_distance_sqr: f32,
    /// The once-per-frame guards (`crossFocusFrame_`/`dragFocusFrame_`,
    /// initialised to -1 so the very first frame ticks).
    cross_focus_frame: i64,
    drag_focus_frame: i64,
    /// `Moo::rc().frameTimestamp()` analog: the once-per-frame guards
    /// compare against this; the host bumps it per frame.
    frame_timestamp: u64,
    mouse_cursor: MouseCursor,
    clip_stack: Vec<ClipRect>,
    tree_cookie: u32,
}

/// `filterList`'s tree-membership marker (`guiTreeCookieRecursive`): mark the
/// whole subtree, one RefCell borrow at a time.
fn mark_tree_cookie(component: &ComponentRef, cookie: u32) {
    let children = component.borrow().children.clone();
    for (_, child) in &children {
        mark_tree_cookie(child, cookie);
    }
    component.borrow_mut().tree_cookie = cookie;
}

/// `calcDrawOrderRecursively` (simple_gui_component.cpp:4520): assign this
/// component the next order, then walk the children in depth order
/// (`reSortChildren` sorts them by z descending), each child consuming a
/// contiguous range. Returns the next free order, or 0 when the range ran
/// out (which makes the caller fall back to a full recalc).
fn calc_draw_order_recursively(
    component: &ComponentRef,
    draw_order: u32,
    next_draw_order: u32,
) -> u32 {
    // reSortChildren: child indices by z, descending.
    let children = component.borrow().children.clone();
    let mut order: Vec<usize> = (0..children.len()).collect();
    order.sort_by(|&i, &j| {
        let zi = children[i].1.borrow().position.z;
        let zj = children[j].1.borrow().position.z;
        zj.partial_cmp(&zi).unwrap_or(std::cmp::Ordering::Equal)
    });

    {
        let mut comp = component.borrow_mut();
        comp.draw_order = draw_order;
    }

    let mut curr = draw_order + 1;

    // Each child needs at least one drawOrder value for itself.
    if next_draw_order > 0 && next_draw_order - curr < children.len() as u32 {
        return 0;
    }

    for &index in &order {
        if next_draw_order > 0 && curr >= next_draw_order {
            return 0;
        }
        curr = calc_draw_order_recursively(&children[index].1, curr, next_draw_order);
        if curr == 0 {
            return 0;
        }
    }

    curr
}

/// Which callback pair `detectEnterLeaveEvents` drives (the `EventForwarder`
/// templates): plain hover uses enter/leave, a drag uses drag-enter/leave
/// with the dragged component and writes the enter return into
/// `targetAccepts`.
enum DetectKind<'a> {
    Hover,
    Drag {
        dragged: &'a ComponentRef,
        target_accepts: &'a mut bool,
    },
}

impl SimpleGui {
    /// The reference's constructor defaults: empty everything,
    /// `dragDistanceSqr_ = c_defaultDrgaDistance^2`, `updateGUI_` true, the
    /// frame guards at -1, `guiTreeCookie_` at 1, and the clip stack seeded
    /// with the full-screen region `Vector4(-1, 1, 1, -1)` — the reference
    /// pushes it in the constructor body and asserts it is the only leftover
    /// after every draw.
    pub fn new() -> SimpleGui {
        SimpleGui {
            components: Vec::new(),
            mouse_button_focus_list: Vec::new(),
            cross_focus_list: Vec::new(),
            drag_focus_list: Vec::new(),
            drop_focus_list: Vec::new(),
            click_component: None,
            drag_info: None,
            current_cross_focus: None,
            current_drop_focus: None,
            drag_distance_sqr: DEFAULT_DRAG_DISTANCE * DEFAULT_DRAG_DISTANCE,
            cross_focus_frame: -1,
            drag_focus_frame: -1,
            frame_timestamp: 0,
            mouse_cursor: MouseCursor {
                position: Vector2::default(),
                active: false,
            },
            clip_stack: vec![ClipRect {
                left: -1.0,
                top: 1.0,
                right: 1.0,
                bottom: -1.0,
            }],
            tree_cookie: 1,
        }
    }

    /// The mouse cursor (routing consults `isActive`/`position`).
    pub fn mouse_cursor(&mut self) -> &mut MouseCursor {
        &mut self.mouse_cursor
    }

    /// Advance the frame timestamp (`Moo::rc().frameTimestamp()`); the
    /// once-per-frame enter/leave guards reset against it.
    pub fn bump_frame(&mut self) {
        self.frame_timestamp += 1;
    }

    /// `GUI.setDragDistance` (:1956): `dragDistanceSqr_ = d * d`.
    pub fn set_drag_distance(&mut self, distance: f32) {
        self.drag_distance_sqr = distance * distance;
    }

    /// The roots, in current draw order (`GUI.roots`).
    pub fn roots(&self) -> Vec<ComponentRef> {
        self.components.clone()
    }

    /// `addSimpleComponent` (:402): append once; a second add is a warning
    /// in the reference and a no-op here.
    pub fn add_root(&mut self, component: ComponentRef) {
        if self.components.iter().any(|c| Rc::ptr_eq(c, &component)) {
            return;
        }
        self.components.push(component);
        self.re_sort();
    }

    /// `removeSimpleComponent` (:427): only components with
    /// `allowDelete()` leave; the focus lists keep their (now stale) entries,
    /// which `filterList`'s tree-membership filter neutralises at routing
    /// time.
    pub fn remove_root(&mut self, component: &ComponentRef) {
        let allow = component.borrow().allow_delete();
        if !allow {
            return;
        }
        if let Some(index) = self
            .components
            .iter()
            .position(|c| Rc::ptr_eq(c, component))
        {
            self.components.remove(index);
            self.re_sort();
        }
    }

    /// `clearSimpleComponents` (:447): drop the roots only — the reference
    /// deliberately leaves the focus lists, exactly as this port does.
    pub fn clear(&mut self) {
        self.components.clear();
    }

    /// `reSort` (:467): roots ordered by z descending (`depth_compare_gui_
    /// components` — the reference's `qsort` never returns 0 so ties order
    /// arbitrarily; this port's stable sort keeps insertion order for ties),
    /// then draw orders recalculated.
    pub fn re_sort(&mut self) {
        self.components.sort_by(|a, b| {
            let za = a.borrow().position.z;
            let zb = b.borrow().position.z;
            zb.partial_cmp(&za).unwrap_or(std::cmp::Ordering::Equal)
        });
        self.recalc_draw_orders();
    }

    /// `recalcDrawOrders` (:1274): roots consume increasing order ranges in
    /// components_ order.
    pub fn recalc_draw_orders(&mut self) {
        let mut curr = 0;
        let roots = self.components.clone();
        for root in &roots {
            curr = calc_draw_order_recursively(root, curr, 0);
        }
    }

    /// Attach a child under an assembled parent (sets the parent
    /// back-pointer `handlesInput` walks).
    pub fn attach_child(
        &mut self,
        owner: &ComponentRef,
        name: impl Into<String>,
        child: ComponentRef,
    ) {
        child.borrow_mut().parent = Some(Rc::downgrade(owner));
        owner.borrow_mut().children.push((name.into(), child));
        self.recalc_draw_orders();
    }

    /// Push a clip region (`pushClipRegion(Vector4)`, simple_gui.cpp):
    /// intersect with the region below, push, and fail — popping again —
    /// when the intersection is empty.
    pub fn push_clip_region(&mut self, region: ClipRect) -> bool {
        let clipped = match self.clip_stack.last() {
            Some(below) => region.intersect(*below),
            None => region,
        };
        self.clip_stack.push(clipped);
        if clipped.is_empty() {
            self.clip_stack.pop();
            return false;
        }
        true
    }

    /// Pop the clip stack (`popClipRegion`).
    pub fn pop_clip_region(&mut self) {
        self.clip_stack.pop();
    }

    /// `isPointInClipRegion`: true with an empty stack, else inclusive bounds
    /// against the top region.
    pub fn is_point_in_clip_region(&self, pt: Vector2) -> bool {
        match self.clip_stack.last() {
            None => true,
            Some(region) => region.contains(pt),
        }
    }

    /// The focus-flag setter (`IMPLEMENT_FOCUS_FUNCTION`): no-op when
    /// unchanged, set the flag, notify the script attribute with an int, then
    /// maintain the matching focus list (with its side effects).
    pub fn set_focus_flag(&mut self, component: &ComponentRef, which: FocusKind, state: bool) {
        if focus_flag(&component.borrow(), which) == state {
            return;
        }
        set_focus_flag(&mut component.borrow_mut(), which, state);
        let script = component.borrow().script.clone();
        if let Some(script) = script {
            script.notify(focus_callback(which), state);
        }
        if state {
            self.add_focus(which, component);
        } else {
            self.del_focus(which, component);
        }
    }

    /// The ADD list functions (`addMouseButtonFocus` etc.); `focus`/`move`
    /// are the EMPTY ones.
    fn add_focus(&mut self, which: FocusKind, component: &ComponentRef) {
        let list = match which {
            FocusKind::Focus | FocusKind::Move => return,
            FocusKind::MouseButton => &mut self.mouse_button_focus_list,
            FocusKind::Cross => &mut self.cross_focus_list,
            FocusKind::Drag => &mut self.drag_focus_list,
            FocusKind::Drop => &mut self.drop_focus_list,
        };
        list.push(component.clone());
    }

    /// The DEL list functions with their side effects
    /// (`delMouseButtonFocus` :1252 clears a matching pending click,
    /// `delMouseDragFocus` :1408 cancels a matching drag entirely,
    /// `delMouseDropFocus` :1428 detaches a matching target and UNDRAGS).
    fn del_focus(&mut self, which: FocusKind, component: &ComponentRef) {
        let list = match which {
            FocusKind::Focus | FocusKind::Move => return,
            FocusKind::MouseButton => {
                self.mouse_button_focus_list
                    .retain(|c| !Rc::ptr_eq(c, component));
                if let Some(click) = &self.click_component
                    && Rc::ptr_eq(click, component)
                {
                    self.click_component = None;
                }
                return;
            }
            FocusKind::Cross => &mut self.cross_focus_list,
            FocusKind::Drag => {
                self.drag_focus_list.retain(|c| !Rc::ptr_eq(c, component));
                if let Some(info) = &self.drag_info
                    && Rc::ptr_eq(&info.component, component)
                {
                    self.drag_info = None;
                }
                return;
            }
            FocusKind::Drop => {
                self.drop_focus_list.retain(|c| !Rc::ptr_eq(c, component));
                if let Some(info) = &mut self.drag_info
                    && info
                        .target
                        .as_ref()
                        .is_some_and(|target| Rc::ptr_eq(target, component))
                {
                    info.target = None;
                    info.dragging = false;
                    info.target_accepts = false;
                }
                return;
            }
        };
        list.retain(|c| !Rc::ptr_eq(c, component));
    }

    /// `filterList` (:922): keep only focus-list entries that still live in
    /// some root's tree — this is what makes stale entries after
    /// `remove_root`/`clear` harmless.
    fn filter_list(&mut self, list: &[ComponentRef]) -> Vec<ComponentRef> {
        self.tree_cookie += 1;
        let cookie = self.tree_cookie;
        let roots = self.components.clone();
        for root in &roots {
            mark_tree_cookie(root, cookie);
        }
        list.iter()
            .filter(|c| c.borrow().tree_cookie == cookie)
            .cloned()
            .collect()
    }

    /// `closestHitTest` (:1030): the focus list filtered to in-tree entries,
    /// walked BACKWARD, keeping the hit with the highest draw order.
    fn closest_hit_test(&mut self, list: &[ComponentRef], pos: Vector2) -> Option<ComponentRef> {
        let filtered = self.filter_list(list);
        let clip_top = self.clip_stack.last().copied();
        let mut best: Option<ComponentRef> = None;
        for component in filtered.iter().rev() {
            if component.borrow().hit_test(clip_top, pos) {
                let order = component.borrow().draw_order;
                let better = match &best {
                    None => true,
                    Some(current) => order > current.borrow().draw_order,
                };
                if better {
                    best = Some(component.clone());
                }
            }
        }
        best
    }

    /// `handleKeyEvent` (:818): mouse-button focus routing, then click, then
    /// drag tracking — and only when nothing handled the event, the roots
    /// LAST-to-FIRST.
    pub fn handle_key_event(&mut self, event: KeyEvent) -> bool {
        let mut handled = false;
        let mouse_pos = event.cursor_position;

        if self.mouse_cursor.active && event.is_mouse_button() {
            let list = self.mouse_button_focus_list.clone();
            if let Some(component) = self.closest_hit_test(&list, mouse_pos) {
                handled = component.borrow().handle_mouse_button_event(event);
            }
            handled |= self.process_click_key(event);
            handled |= self.process_drag_key(event);
        }

        if !handled {
            for index in (0..self.components.len()).rev() {
                let component = self.components[index].clone();
                handled = component.borrow().handle_key_event(event);
                if handled {
                    break;
                }
            }
        }

        handled
    }

    /// `handleMouseEvent` (:863): drag-move first, then the plain move —
    /// gated on the cursor being active. (The reference reads
    /// `mouseCursor().position()` into a local it never uses; routing runs
    /// on the event's own position.)
    pub fn handle_mouse_event(&mut self, event: MouseEvent) -> bool {
        if !self.mouse_cursor.active {
            return false;
        }
        if self.process_drag_move(event) {
            return true;
        }
        self.process_mouse_move(event)
    }

    /// `handleAxisEvent` (:896): the roots LAST-to-FIRST, no focus list.
    pub fn handle_axis_event(&mut self, event: AxisEvent) -> bool {
        let mut handled = false;
        for index in (0..self.components.len()).rev() {
            let component = self.components[index].clone();
            handled = component.borrow().handle_axis_event(event);
            if handled {
                break;
            }
        }
        handled
    }

    /// `processClickKey` (:1063): left-down captures the closest hit over the
    /// mouse-button focus list; left-up fires the click only when no drag is
    /// under way AND the up position still hits the captured component — an
    /// up elsewhere leaves the capture pending.
    fn process_click_key(&mut self, event: KeyEvent) -> bool {
        let mut handled = false;

        if event.key == KEY_LEFTMOUSE {
            if event.is_key_down() {
                let list = self.mouse_button_focus_list.clone();
                self.click_component = self.closest_hit_test(&list, event.cursor_position);
            } else {
                let dragging = matches!(&self.drag_info, Some(info) if info.dragging);
                if let Some(component) = &self.click_component
                    && !dragging
                    && component
                        .borrow()
                        .hit_test(self.clip_stack.last().copied(), event.cursor_position)
                {
                    let component = component.clone();
                    handled = component.borrow().handle_mouse_click_event(event);
                    self.click_component = None;
                }
            }
        }

        handled
    }

    /// `processDragKey` (:1092): left-down on a drag-focus component opens a
    /// `DragInfo` (clearing the drop focus); left-up with a dragging gesture
    /// fires the drop on the target (only when it accepted the drag enter)
    /// and then the drag stop on the dragged component. The reference
    /// re-checks `dragInfo` between the two calls because the drop handler
    /// may delete the dragged component — `Rc` ownership makes that
    /// impossible here. Returns whether a `DragInfo` is open.
    fn process_drag_key(&mut self, event: KeyEvent) -> bool {
        if event.key == KEY_LEFTMOUSE {
            if event.is_key_down() {
                let list = self.drag_focus_list.clone();
                if let Some(component) = self.closest_hit_test(&list, event.cursor_position) {
                    self.drag_info = Some(DragInfo {
                        key_event: event,
                        component,
                        target: None,
                        dragging: false,
                        target_accepts: false,
                    });
                    self.current_drop_focus = None;
                }
            } else if self.drag_info.is_some() {
                let info = self.drag_info.take().unwrap();
                if info.dragging
                    && let Some(target) = &info.target
                    && info.target_accepts
                {
                    target.borrow().handle_drop_event(&info.component, event);
                }
                if info.dragging {
                    info.component.borrow().handle_drag_stop_event(event);
                }
                self.current_drop_focus = None;
            }
        }

        self.drag_info.is_some()
    }

    /// `processMouseMove` (:1172): enter/leave ticks run first ("when the
    /// mouse moves we want enter/leave to happen before the
    /// handleMouseEvent on this frame"), then the roots LAST-to-FIRST — a
    /// root whose quad the position hits gets `handleMouseEvent`.
    fn process_mouse_move(&mut self, event: MouseEvent) -> bool {
        self.tick_cross_focus_events();
        self.tick_drag_focus_events();

        let clip_top = self.clip_stack.last().copied();
        for index in (0..self.components.len()).rev() {
            let component = self.components[index].clone();
            if component.borrow().hit_test(clip_top, event.cursor_position)
                && component.borrow().handle_mouse_event(clip_top, event)
            {
                return true;
            }
        }

        false
    }

    /// `processDragMove` (:1190): a captured but not-yet-dragging gesture
    /// starts once the move crosses `dragDistanceSqr_` from the BUTTON-DOWN
    /// position; a declined `handleDragStartEvent` aborts the whole drag.
    /// Returns whether a `DragInfo` is open.
    fn process_drag_move(&mut self, event: MouseEvent) -> bool {
        let captured = matches!(&self.drag_info, Some(info) if !info.dragging);
        if captured {
            let start = self.drag_info.as_ref().unwrap().key_event.cursor_position;
            if (start - event.cursor_position).length_squared() > self.drag_distance_sqr {
                let component = self.drag_info.as_ref().unwrap().component.clone();
                let key_event = self.drag_info.as_ref().unwrap().key_event;
                if component.borrow().handle_drag_start_event(key_event) {
                    self.drag_info.as_mut().unwrap().dragging = true;
                } else {
                    self.drag_info = None;
                }
            }
        }

        self.drag_info.is_some()
    }

    /// `tickCrossFocusEvents` (:1297): once per frame timestamp, hover
    /// enter/leave over the cross focus list. The reference calls this pair
    /// from `processMouseMove` AND from `draw()`'s tail — the drop detection
    /// during a drag lives on the draw tail, because `processDragMove`
    /// short-circuits `processMouseMove` for the whole gesture.
    pub fn tick_cross_focus_events(&mut self) {
        if self.cross_focus_frame == self.frame_timestamp as i64 {
            return;
        }
        self.cross_focus_frame = self.frame_timestamp as i64;

        let list = self.cross_focus_list.clone();
        let current = self.current_cross_focus.clone();
        self.current_cross_focus =
            self.detect_enter_leave_events(&list, current, &mut DetectKind::Hover);
    }

    /// `tickDragFocusEvents` (:1313): only while a drag is under way; drop
    /// enter/leave over the drop focus list, the enter return written into
    /// `targetAccepts`, and the detected component published as the target.
    pub fn tick_drag_focus_events(&mut self) {
        let dragging = matches!(&self.drag_info, Some(info) if info.dragging);
        if self.drag_focus_frame == self.frame_timestamp as i64 || !dragging {
            return;
        }
        self.drag_focus_frame = self.frame_timestamp as i64;

        let list = self.drop_focus_list.clone();
        let current = self.current_drop_focus.clone();
        let dragged = self.drag_info.as_ref().unwrap().component.clone();
        let mut accepts = self.drag_info.as_ref().unwrap().target_accepts;
        self.current_drop_focus = self.detect_enter_leave_events(
            &list,
            current,
            &mut DetectKind::Drag {
                dragged: &dragged,
                target_accepts: &mut accepts,
            },
        );
        if let Some(info) = self.drag_info.as_mut() {
            info.target = self.current_drop_focus.clone();
            info.target_accepts = accepts;
        }
    }

    /// `detectEnterLeaveEvents` (:953): with an active cursor, find the
    /// top-most in-tree focus-list entry under the CURSOR position (sorted by
    /// draw order descending); with an inactive cursor nothing is over. On a
    /// change, the leave fires on the OLD component first, then the enter on
    /// the NEW one — both with a zeroed event at the detection position
    /// (out-of-bounds when the cursor is inactive).
    fn detect_enter_leave_events(
        &mut self,
        list: &[ComponentRef],
        current_over: Option<ComponentRef>,
        kind: &mut DetectKind,
    ) -> Option<ComponentRef> {
        const OUT_OF_BOUNDS: Vector2 = Vector2 {
            x: f32::MAX,
            y: f32::MAX,
        };

        let mut mouse_pos = OUT_OF_BOUNDS;
        let mut over: Option<ComponentRef> = None;

        if self.mouse_cursor.active {
            mouse_pos = self.mouse_cursor.position;

            let mut filtered = self.filter_list(list);
            filtered.sort_by(|a, b| {
                let da = a.borrow().draw_order;
                let db = b.borrow().draw_order;
                db.cmp(&da)
            });
            let clip_top = self.clip_stack.last().copied();
            for component in &filtered {
                if over.is_none() && component.borrow().hit_test(clip_top, mouse_pos) {
                    over = Some(component.clone());
                }
            }
        }

        let changed = match (&over, &current_over) {
            (Some(new), Some(old)) => !Rc::ptr_eq(new, old),
            (None, None) => false,
            _ => true,
        };

        if changed {
            let event = MouseEvent::fill(0, 0, 0, mouse_pos);
            match kind {
                DetectKind::Hover => {
                    if let Some(old) = &current_over {
                        old.borrow().handle_mouse_leave_event(event);
                    }
                    if let Some(new) = &over {
                        new.borrow().handle_mouse_enter_event(event);
                    }
                }
                DetectKind::Drag {
                    dragged,
                    target_accepts,
                } => {
                    if let Some(old) = &current_over {
                        old.borrow().handle_drag_leave_event(dragged, event);
                    }
                    if let Some(new) = &over {
                        **target_accepts = new.borrow().handle_drag_enter_event(dragged, event);
                    }
                }
            }
        }

        over
    }
}

impl Default for SimpleGui {
    fn default() -> SimpleGui {
        SimpleGui::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh gui with an active cursor at the given position.
    fn gui_with_cursor_at(pos: Vector2) -> SimpleGui {
        let mut gui = SimpleGui::new();
        let cursor = gui.mouse_cursor();
        cursor.active = true;
        cursor.position = pos;
        gui
    }

    /// A positioned, sized component with a script bound to the log.
    fn scripted_component(
        log: &ScriptLog,
        name: &str,
        position: Vector3,
        size: (f32, f32),
    ) -> ComponentRef {
        let component = SimpleGuiComponent::new(name);
        component
            .borrow_mut()
            .set_script(ScriptObject::new(name, log));
        component.borrow_mut().set_position(position);
        component.borrow_mut().set_size(size.0, size.1);
        component
    }

    fn left_down(pos: Vector2) -> KeyEvent {
        KeyEvent {
            key: KEY_LEFTMOUSE,
            state: 0,
            modifiers: 0,
            cursor_position: pos,
        }
    }

    fn left_up(pos: Vector2) -> KeyEvent {
        KeyEvent {
            key: KEY_LEFTMOUSE,
            state: -1,
            modifiers: 0,
            cursor_position: pos,
        }
    }

    fn move_event(pos: Vector2) -> MouseEvent {
        MouseEvent::fill(1, 1, 0, pos)
    }

    /// One frame's tail of `SimpleGUI::draw()`: the enter/leave ticks run on
    /// the CURSOR position (which is separate from the mouse event's).
    fn draw_tail(gui: &mut SimpleGui, cursor_pos: Vector2) {
        gui.bump_frame();
        gui.mouse_cursor().position = cursor_pos;
        gui.tick_cross_focus_events();
        gui.tick_drag_focus_events();
    }

    fn names(calls: &[ScriptCall]) -> Vec<(String, String)> {
        calls
            .iter()
            .map(|c| (c.component.clone(), c.callback.clone()))
            .collect()
    }

    /// The focus-flag setter notifications ("focus", "mouseButtonFocus", ...)
    /// share the log with the routed events; routing assertions want only
    /// the events.
    const FLAG_SETTERS: [&str; 6] = [
        "focus",
        "mouseButtonFocus",
        "moveFocus",
        "crossFocus",
        "dragFocus",
        "dropFocus",
    ];

    fn routing_log(log: &ScriptLog) -> Vec<ScriptCall> {
        log.borrow()
            .iter()
            .filter(|c| !FLAG_SETTERS.contains(&c.callback.as_str()))
            .cloned()
            .collect()
    }

    /// A component quad at (0.4, 0.4) sized 0.2 covers x 0.4..0.6, y
    /// 0.2..0.4 (y grows down: the top edge carries the larger y).
    const POS: Vector3 = Vector3 {
        x: 0.4,
        y: 0.4,
        z: 0.5,
    };
    const SIZE: (f32, f32) = (0.2, 0.2);
    const INSIDE: Vector2 = Vector2 { x: 0.5, y: 0.3 };
    const OUTSIDE: Vector2 = Vector2 { x: 0.9, y: 0.9 };

    #[test]
    fn mouse_button_down_routes_to_the_closest_hit_in_focus_order() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        // Two overlapping roots, both on the mouse-button focus list. The
        // deeper z (0.1) gets the higher draw order, so it is on top and
        // wins the closest hit.
        let back = scripted_component(&log, "back", Vector3 { z: 0.5, ..POS }, SIZE);
        let front = scripted_component(&log, "front", Vector3 { z: 0.1, ..POS }, SIZE);
        gui.add_root(back.clone());
        gui.add_root(front.clone());
        gui.set_focus_flag(&back, FocusKind::MouseButton, true);
        gui.set_focus_flag(&front, FocusKind::MouseButton, true);
        ScriptObject::set_handled(
            front.borrow().script().unwrap(),
            "handleMouseButtonEvent",
            true,
        );

        let handled = gui.handle_key_event(left_down(INSIDE));

        assert!(handled);
        let calls = routing_log(&log);
        assert_eq!(
            names(&calls),
            vec![("front".to_string(), "handleMouseButtonEvent".to_string())]
        );
        // invokeKeyEventHandler's tuple: (comp, event) — the component first,
        // then the full key event.
        assert_eq!(
            calls[0].args,
            vec![
                ScriptArg::Component("front".to_string()),
                ScriptArg::Key(left_down(INSIDE))
            ]
        );
        assert_eq!(front.borrow().draw_order(), 1);
        assert_eq!(back.borrow().draw_order(), 0);
    }

    #[test]
    fn key_events_fall_through_roots_last_to_first_and_only_focus_answers() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(OUTSIDE);

        // No mouse button focus anywhere, cursor far from everything.
        let bottom = scripted_component(&log, "bottom", Vector3 { z: 0.1, ..POS }, SIZE);
        let top = scripted_component(&log, "top", Vector3 { z: 0.9, ..POS }, SIZE);
        gui.add_root(bottom.clone());
        gui.add_root(top.clone());
        // "top" has focus and declines; "bottom" has a script but no focus.
        gui.set_focus_flag(&top, FocusKind::Focus, true);

        let handled = gui.handle_key_event(left_down(INSIDE));

        assert!(!handled);
        // Roots walked LAST-to-FIRST: "bottom" (lowest z, highest draw
        // order) first — but without focus() its script is NEVER consulted;
        // then "top", whose focus() lets handleKeyEvent run (and decline).
        assert_eq!(
            names(&routing_log(&log)),
            vec![("top".to_string(), "handleKeyEvent".to_string())]
        );
        // The key handler tuple is (event) alone — no component argument.
        assert_eq!(
            routing_log(&log)[0].args,
            vec![ScriptArg::Key(left_down(INSIDE))]
        );
    }

    #[test]
    fn children_get_key_events_before_their_parent() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(OUTSIDE);

        let parent = scripted_component(&log, "parent", POS, SIZE);
        gui.add_root(parent.clone());
        gui.set_focus_flag(&parent, FocusKind::Focus, true);
        let child = scripted_component(&log, "child", POS, SIZE);
        gui.attach_child(&parent, "child", child.clone());
        gui.set_focus_flag(&child, FocusKind::Focus, true);
        // Child accepts; the parent must never see the event.
        ScriptObject::set_handled(child.borrow().script().unwrap(), "handleKeyEvent", true);

        let handled = gui.handle_key_event(left_down(INSIDE));

        assert!(handled);
        assert_eq!(
            names(&routing_log(&log)),
            vec![("child".to_string(), "handleKeyEvent".to_string())]
        );
    }

    #[test]
    fn a_click_is_down_then_up_over_the_same_component() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "clicky", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        ScriptObject::set_handled(
            comp.borrow().script().unwrap(),
            "handleMouseClickEvent",
            true,
        );

        gui.handle_key_event(left_down(INSIDE));
        gui.handle_key_event(left_up(INSIDE));

        // The click tuple is (comp) alone — the position the docstrings
        // promise is computed but never passed.
        let calls = routing_log(&log);
        assert_eq!(
            names(&calls),
            vec![
                ("clicky".to_string(), "handleMouseButtonEvent".to_string()),
                ("clicky".to_string(), "handleMouseButtonEvent".to_string()),
                ("clicky".to_string(), "handleMouseClickEvent".to_string()),
            ]
        );
        assert_eq!(
            calls[2].args,
            vec![ScriptArg::Component("clicky".to_string())]
        );
    }

    #[test]
    fn an_up_outside_keeps_the_pending_click() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "clicky", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        ScriptObject::set_handled(
            comp.borrow().script().unwrap(),
            "handleMouseClickEvent",
            true,
        );

        gui.handle_key_event(left_down(INSIDE));
        // Released over another component entirely: no click callback, and
        // the capture stays pending (the reference only resets it inside the
        // hit branch).
        gui.handle_key_event(left_up(OUTSIDE));
        assert!(
            names(&routing_log(&log))
                .iter()
                .all(|(_, cb)| cb == "handleMouseButtonEvent")
        );
        // A later up inside still clicks.
        gui.handle_key_event(left_up(INSIDE));
        assert!(
            names(&routing_log(&log))
                .iter()
                .any(|(_, cb)| cb == "handleMouseClickEvent")
        );
    }

    #[test]
    fn an_active_drag_suppresses_the_click() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "draggy", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        gui.set_focus_flag(&comp, FocusKind::Drag, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleDragStartEvent", true);

        gui.handle_key_event(left_down(INSIDE));
        // A move far past the default 0.002 drag distance starts the drag.
        let far = Vector2 {
            x: INSIDE.x + 0.5,
            y: INSIDE.y,
        };
        assert!(gui.handle_mouse_event(move_event(far)));
        gui.handle_key_event(left_up(far));

        // No handleMouseClickEvent: a click is not a click once a drag
        // happened; the pending capture is simply left alone.
        assert!(
            names(&routing_log(&log))
                .iter()
                .all(|(_, cb)| cb != "handleMouseClickEvent")
        );
        assert!(
            names(&routing_log(&log))
                .iter()
                .any(|(_, cb)| cb == "handleDragStartEvent")
        );
        assert!(
            names(&routing_log(&log))
                .iter()
                .any(|(_, cb)| cb == "handleDragStopEvent")
        );
    }

    #[test]
    fn drag_start_requires_crossing_the_drag_distance() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);
        gui.set_drag_distance(0.1);

        let comp = scripted_component(&log, "draggy", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::Drag, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleDragStartEvent", true);

        gui.handle_key_event(left_down(INSIDE));
        // A move within 0.1 of the down position: captured but not dragging.
        let near = Vector2 {
            x: INSIDE.x + 0.05,
            y: INSIDE.y,
        };
        assert!(gui.handle_mouse_event(move_event(near)));
        // Nothing fired: below the distance the gesture is not under way,
        // and the drag move swallowed the frame (no move routing either).
        assert!(names(&routing_log(&log)).is_empty());

        // Crossing the distance fires the start with the BUTTON-DOWN event —
        // the (comp) tuple carries no position at all.
        let far = Vector2 {
            x: INSIDE.x + 0.2,
            y: INSIDE.y,
        };
        assert!(gui.handle_mouse_event(move_event(far)));
        let calls = routing_log(&log);
        let start = calls
            .iter()
            .find(|c| c.callback == "handleDragStartEvent")
            .expect("drag started");
        assert_eq!(start.component, "draggy");
        assert_eq!(start.args, vec![ScriptArg::Component("draggy".to_string())]);
    }

    #[test]
    fn a_declined_drag_start_aborts_the_drag() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "draggy", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::Drag, true);
        // Default handled = false: the component refuses the drag.

        gui.handle_key_event(left_down(INSIDE));
        let far = Vector2 {
            x: INSIDE.x + 0.5,
            y: INSIDE.y,
        };
        // The declined start aborts the gesture: processDragMove answers
        // false and no further drag callbacks can ever fire.
        assert!(!gui.handle_mouse_event(move_event(far)));
        gui.handle_key_event(left_up(far));
        assert_eq!(
            names(&routing_log(&log)),
            vec![("draggy".to_string(), "handleDragStartEvent".to_string())]
        );
    }

    #[test]
    fn drop_then_drag_stop_fire_in_that_order_on_button_up() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let dragged = scripted_component(&log, "dragged", POS, SIZE);
        gui.add_root(dragged.clone());
        gui.set_focus_flag(&dragged, FocusKind::Drag, true);
        let dragged_script = dragged.borrow().script().unwrap().clone();
        dragged_script.set_handled("handleDragStartEvent", true);

        // The target sits elsewhere with drop focus.
        let target_pos = Vector3 {
            x: 0.7,
            y: 0.4,
            z: 0.1,
        };
        let target = scripted_component(&log, "target", target_pos, (0.2, 0.2));
        gui.add_root(target.clone());
        gui.set_focus_flag(&target, FocusKind::Drop, true);
        let target_script = target.borrow().script().unwrap().clone();
        target_script.set_handled("handleDragEnterEvent", true);

        let over_target = Vector2 { x: 0.8, y: 0.3 };
        gui.handle_key_event(left_down(INSIDE));
        gui.handle_mouse_event(move_event(over_target)); // crosses: drag starts
        draw_tail(&mut gui, over_target); // draw tail: drag enter, accepted
        gui.handle_key_event(left_up(over_target)); // drops + stops

        let calls = names(&routing_log(&log));
        assert_eq!(
            calls,
            vec![
                ("dragged".to_string(), "handleDragStartEvent".to_string()),
                ("target".to_string(), "handleDragEnterEvent".to_string()),
                ("target".to_string(), "handleDropEvent".to_string()),
                ("dragged".to_string(), "handleDragStopEvent".to_string()),
            ]
        );
        // Drag enter/leave and drop tuples are (comp, dragged).
        let drop = &routing_log(&log)[2];
        assert_eq!(
            drop.args,
            vec![
                ScriptArg::Component("target".to_string()),
                ScriptArg::Component("dragged".to_string()),
            ]
        );
    }

    #[test]
    fn a_declined_drag_enter_means_no_drop() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let dragged = scripted_component(&log, "dragged", POS, SIZE);
        gui.add_root(dragged.clone());
        gui.set_focus_flag(&dragged, FocusKind::Drag, true);
        let dragged_script = dragged.borrow().script().unwrap().clone();
        dragged_script.set_handled("handleDragStartEvent", true);

        let target_pos = Vector3 {
            x: 0.7,
            y: 0.4,
            z: 0.1,
        };
        let target = scripted_component(&log, "target", target_pos, (0.2, 0.2));
        gui.add_root(target.clone());
        gui.set_focus_flag(&target, FocusKind::Drop, true);
        // Default false: the target refuses the drag enter.

        let over_target = Vector2 { x: 0.8, y: 0.3 };
        gui.handle_key_event(left_down(INSIDE));
        gui.handle_mouse_event(move_event(over_target)); // drag starts
        draw_tail(&mut gui, over_target); // enter fires, declines (default)
        gui.handle_key_event(left_up(over_target));

        let callbacks: Vec<String> = names(&routing_log(&log))
            .into_iter()
            .map(|(_, cb)| cb)
            .collect();
        assert!(callbacks.contains(&"handleDragEnterEvent".to_string()));
        assert!(!callbacks.contains(&"handleDropEvent".to_string()));
        assert!(callbacks.contains(&"handleDragStopEvent".to_string()));
    }

    #[test]
    fn drag_leave_fires_when_the_drag_moves_off_the_target() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let dragged = scripted_component(&log, "dragged", POS, SIZE);
        gui.add_root(dragged.clone());
        gui.set_focus_flag(&dragged, FocusKind::Drag, true);
        let dragged_script = dragged.borrow().script().unwrap().clone();
        dragged_script.set_handled("handleDragStartEvent", true);

        let target_pos = Vector3 {
            x: 0.7,
            y: 0.4,
            z: 0.1,
        };
        let target = scripted_component(&log, "target", target_pos, (0.2, 0.2));
        gui.add_root(target.clone());
        gui.set_focus_flag(&target, FocusKind::Drop, true);
        let target_script = target.borrow().script().unwrap().clone();
        target_script.set_handled("handleDragEnterEvent", true);

        let over_target = Vector2 { x: 0.8, y: 0.3 };
        gui.handle_key_event(left_down(INSIDE));
        gui.handle_mouse_event(move_event(over_target)); // drag starts
        draw_tail(&mut gui, over_target); // enter
        // Moving off during a drag short-circuits processMouseMove — the
        // leave fires on the draw tail's tick.
        gui.handle_mouse_event(move_event(OUTSIDE));
        draw_tail(&mut gui, OUTSIDE); // leave, target cleared
        gui.handle_key_event(left_up(OUTSIDE)); // no drop

        let callbacks: Vec<String> = names(&routing_log(&log))
            .into_iter()
            .map(|(_, cb)| cb)
            .collect();
        assert_eq!(
            callbacks,
            vec![
                "handleDragStartEvent".to_string(),
                "handleDragEnterEvent".to_string(),
                "handleDragLeaveEvent".to_string(),
                "handleDragStopEvent".to_string(),
            ]
        );
        // The leave tuple is also (comp, dragged).
        assert_eq!(
            routing_log(&log)[2].args,
            vec![
                ScriptArg::Component("target".to_string()),
                ScriptArg::Component("dragged".to_string()),
            ]
        );
    }

    #[test]
    fn enter_then_leave_order_with_a_zeroed_event() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        // Two non-overlapping cross-focus components.
        let a = scripted_component(&log, "a", POS, SIZE);
        let b_pos = Vector3 {
            x: 0.7,
            y: 0.4,
            z: 0.5,
        };
        let b = scripted_component(&log, "b", b_pos, (0.2, 0.2));
        gui.add_root(a.clone());
        gui.add_root(b.clone());
        gui.set_focus_flag(&a, FocusKind::Cross, true);
        gui.set_focus_flag(&b, FocusKind::Cross, true);

        let over_a = INSIDE;
        let over_b = Vector2 { x: 0.8, y: 0.3 };

        // A mouse move ticks the enter/leave detection BEFORE routing, and
        // the detection looks at the CURSOR position, not the event's.
        gui.mouse_cursor().position = over_a;
        gui.bump_frame();
        gui.handle_mouse_event(move_event(over_a));
        gui.mouse_cursor().position = over_b;
        gui.bump_frame();
        gui.handle_mouse_event(move_event(over_b));

        let calls = names(&routing_log(&log));
        assert_eq!(
            calls,
            vec![
                ("a".to_string(), "handleMouseEnterEvent".to_string()),
                ("a".to_string(), "handleMouseLeaveEvent".to_string()),
                ("b".to_string(), "handleMouseEnterEvent".to_string()),
            ]
        );
        // Leave on the old component fires BEFORE the enter on the new one,
        // and the detection event is zeroed deltas at the cursor position.
        let enter = &routing_log(&log)[0];
        assert_eq!(enter.args, vec![ScriptArg::Component("a".to_string())]);
    }

    #[test]
    fn deactivating_the_cursor_fires_leave_at_the_out_of_bounds_position() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let a = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(a.clone());
        gui.set_focus_flag(&a, FocusKind::Cross, true);

        gui.mouse_cursor().position = INSIDE;
        gui.bump_frame();
        gui.handle_mouse_event(move_event(INSIDE)); // enter
        // Hide the cursor: the routing gate refuses everything, so the leave
        // fires on the draw tail's tick — with the out-of-bounds fill
        // position, since detection never saw a position.
        gui.mouse_cursor().active = false;
        gui.bump_frame();
        gui.tick_cross_focus_events();

        let calls = routing_log(&log);
        assert_eq!(
            names(&calls),
            vec![
                ("a".to_string(), "handleMouseEnterEvent".to_string()),
                ("a".to_string(), "handleMouseLeaveEvent".to_string()),
            ]
        );
    }

    #[test]
    fn enter_leave_ticks_run_once_per_frame() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let a = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(a.clone());
        gui.set_focus_flag(&a, FocusKind::Cross, true);

        // Two moves in the same frame: only the first tick fires the enter.
        gui.mouse_cursor().position = INSIDE;
        gui.bump_frame();
        gui.handle_mouse_event(move_event(INSIDE));
        gui.handle_mouse_event(move_event(INSIDE));
        assert_eq!(names(&routing_log(&log)).len(), 1);

        // The next frame's first move ticks again — but nothing changed, so
        // still no event.
        gui.bump_frame();
        gui.handle_mouse_event(move_event(INSIDE));
        assert_eq!(names(&routing_log(&log)).len(), 1);
    }

    #[test]
    fn mouse_moves_require_an_active_cursor() {
        let mut gui = SimpleGui::new();
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::Move, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleMouseEvent", true);

        // Inactive cursor: nothing routes.
        assert!(!gui.handle_mouse_event(move_event(INSIDE)));
        assert!(routing_log(&log).is_empty());
    }

    #[test]
    fn mouse_move_routing_hits_children_under_the_cursor_first() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let parent = scripted_component(&log, "parent", POS, SIZE);
        gui.add_root(parent.clone());
        gui.set_focus_flag(&parent, FocusKind::Move, true);
        let child = scripted_component(&log, "child", POS, SIZE);
        gui.attach_child(&parent, "child", child.clone());
        gui.set_focus_flag(&child, FocusKind::Move, true);
        let child_script = child.borrow().script().unwrap().clone();
        child_script.set_handled("handleMouseEvent", true);

        gui.handle_mouse_event(move_event(INSIDE));

        // The child (under the cursor) accepted; the parent's handler never
        // ran, and the move tuple is (comp, event).
        let calls = routing_log(&log);
        assert_eq!(
            names(&calls),
            vec![("child".to_string(), "handleMouseEvent".to_string())]
        );
        assert_eq!(
            calls[0].args,
            vec![
                ScriptArg::Component("child".to_string()),
                ScriptArg::Mouse(move_event(INSIDE)),
            ]
        );
    }

    #[test]
    fn the_move_handler_requires_the_move_focus_flag_and_a_hit() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(comp.clone());
        // No moveFocus flag: even a hit runs nothing.
        gui.handle_mouse_event(move_event(INSIDE));
        assert!(routing_log(&log).is_empty());

        gui.set_focus_flag(&comp, FocusKind::Move, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleMouseEvent", true);
        // A position far off the quad: hitTest gates the handler too.
        gui.handle_mouse_event(move_event(OUTSIDE));
        assert!(routing_log(&log).is_empty());

        gui.handle_mouse_event(move_event(INSIDE));
        assert_eq!(names(&routing_log(&log)).len(), 1);
    }

    #[test]
    fn axis_events_walk_the_roots_back_to_front() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(OUTSIDE);

        let bottom = scripted_component(&log, "bottom", Vector3 { z: 0.1, ..POS }, SIZE);
        let top = scripted_component(&log, "top", Vector3 { z: 0.9, ..POS }, SIZE);
        gui.add_root(bottom.clone());
        gui.add_root(top.clone());
        gui.set_focus_flag(&bottom, FocusKind::Focus, true);
        gui.set_focus_flag(&top, FocusKind::Focus, true);
        let bottom_script = bottom.borrow().script().unwrap().clone();
        bottom_script.set_handled("handleAxisEvent", true);

        let axis = AxisEvent {
            axis: 0,
            value: 0.5,
            dtime: 0.016,
        };
        assert!(gui.handle_axis_event(axis));

        // "top" (the last root) declined; "bottom" accepted — so the walk
        // stopped there and the tuple carried the axis event alone.
        let calls = routing_log(&log);
        assert_eq!(
            names(&calls),
            vec![("bottom".to_string(), "handleAxisEvent".to_string())]
        );
        assert_eq!(calls[0].args, vec![ScriptArg::Axis(axis)]);
    }

    #[test]
    fn focus_flag_setters_notify_the_script_and_maintain_the_lists() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(OUTSIDE);

        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(comp.clone());

        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        gui.set_focus_flag(&comp, FocusKind::Cross, true);
        gui.set_focus_flag(&comp, FocusKind::Drag, true);
        gui.set_focus_flag(&comp, FocusKind::Drop, true);
        gui.set_focus_flag(&comp, FocusKind::Move, true);
        gui.set_focus_flag(&comp, FocusKind::Focus, true);

        // One notification per flag, in setter order, each with a single int.
        let calls = log.borrow();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[0].callback, "mouseButtonFocus");
        assert_eq!(calls[0].args, vec![ScriptArg::Int(1)]);
        assert_eq!(calls[1].callback, "crossFocus");
        assert_eq!(calls[4].callback, "moveFocus");
        assert_eq!(calls[5].callback, "focus");
        drop(calls);

        assert!(comp.borrow().mouse_button_focus());
        assert!(comp.borrow().cross_focus());
        assert!(comp.borrow().drag_focus());
        assert!(comp.borrow().drop_focus());
        assert!(comp.borrow().move_focus());
        assert!(comp.borrow().focus());

        // Setting the same value again is a no-op (no extra notification).
        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        assert_eq!(log.borrow().len(), 6);

        gui.set_focus_flag(&comp, FocusKind::MouseButton, false);
        assert_eq!(log.borrow()[6].args, vec![ScriptArg::Int(0)]);
        assert!(!comp.borrow().mouse_button_focus());

        // The remaining del side effects: Focus/Move dels are the no-op
        // list kinds; Cross falls through to the plain retain.
        gui.set_focus_flag(&comp, FocusKind::Move, false);
        gui.set_focus_flag(&comp, FocusKind::Focus, false);
        gui.set_focus_flag(&comp, FocusKind::Cross, false);
        assert!(!comp.borrow().move_focus());
        assert!(!comp.borrow().focus());
        assert!(!comp.borrow().cross_focus());
        assert!(gui.cross_focus_list.is_empty());
    }

    #[test]
    fn removing_mouse_button_focus_clears_the_pending_click() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        ScriptObject::set_handled(
            comp.borrow().script().unwrap(),
            "handleMouseClickEvent",
            true,
        );

        gui.handle_key_event(left_down(INSIDE));
        gui.set_focus_flag(&comp, FocusKind::MouseButton, false);
        // The pending click went with the focus: the up inside fires nothing.
        gui.handle_key_event(left_up(INSIDE));
        assert!(
            !names(&routing_log(&log))
                .iter()
                .any(|(_, cb)| cb == "handleMouseClickEvent")
        );
    }

    #[test]
    fn removing_drag_focus_mid_drag_cancels_the_gesture() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::Drag, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleDragStartEvent", true);

        gui.handle_key_event(left_down(INSIDE));
        let far = Vector2 {
            x: INSIDE.x + 0.5,
            y: INSIDE.y,
        };
        gui.handle_mouse_event(move_event(far)); // dragging now

        gui.set_focus_flag(&comp, FocusKind::Drag, false);
        // The whole DragInfo went with the focus: no drop, no drag stop.
        gui.handle_key_event(left_up(far));
        let callbacks: Vec<String> = names(&routing_log(&log))
            .into_iter()
            .map(|(_, cb)| cb)
            .collect();
        assert!(!callbacks.contains(&"handleDragStopEvent".to_string()));
        assert!(!gui.handle_mouse_event(move_event(far)));
    }

    #[test]
    fn removing_drop_focus_detaches_the_target_and_un_drags() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let dragged = scripted_component(&log, "dragged", POS, SIZE);
        gui.add_root(dragged.clone());
        gui.set_focus_flag(&dragged, FocusKind::Drag, true);
        let dragged_script = dragged.borrow().script().unwrap().clone();
        dragged_script.set_handled("handleDragStartEvent", true);

        let target_pos = Vector3 {
            x: 0.7,
            y: 0.4,
            z: 0.1,
        };
        let target = scripted_component(&log, "target", target_pos, (0.2, 0.2));
        gui.add_root(target.clone());
        gui.set_focus_flag(&target, FocusKind::Drop, true);
        let target_script = target.borrow().script().unwrap().clone();
        target_script.set_handled("handleDragEnterEvent", true);

        let over_target = Vector2 { x: 0.8, y: 0.3 };
        gui.handle_key_event(left_down(INSIDE));
        gui.handle_mouse_event(move_event(over_target)); // dragging
        draw_tail(&mut gui, over_target); // entered, accepted

        // delMouseDropFocus: target cleared AND dragging reset — so the up
        // fires neither the drop nor even the drag stop.
        gui.set_focus_flag(&target, FocusKind::Drop, false);
        gui.handle_key_event(left_up(over_target));

        let callbacks: Vec<String> = names(&routing_log(&log))
            .into_iter()
            .map(|(_, cb)| cb)
            .collect();
        assert!(!callbacks.contains(&"handleDropEvent".to_string()));
        assert!(!callbacks.contains(&"handleDragStopEvent".to_string()));
    }

    #[test]
    fn handles_input_walks_up_the_parent_chain() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let parent = scripted_component(&log, "parent", POS, SIZE);
        gui.add_root(parent.clone());
        let child = scripted_component(&log, "child", POS, SIZE);
        gui.attach_child(&parent, "child", child.clone());
        gui.set_focus_flag(&child, FocusKind::Move, true);
        let script = child.borrow().script().unwrap().clone();
        script.set_handled("handleMouseEvent", true);

        // An invisible parent makes the visible child deaf.
        parent.borrow_mut().set_visible(false);
        assert!(!child.borrow().handles_input());
        assert!(!child.borrow().hit_test(None, INSIDE));
        gui.handle_mouse_event(move_event(INSIDE));
        assert!(routing_log(&log).is_empty());

        parent.borrow_mut().set_visible(true);
        assert!(child.borrow().handles_input());
        gui.handle_mouse_event(move_event(INSIDE));
        assert_eq!(names(&routing_log(&log)).len(), 1);
    }

    #[test]
    fn focus_list_entries_outside_the_tree_stop_routing() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        gui.set_focus_flag(&comp, FocusKind::Cross, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleMouseButtonEvent", true);
        script.set_handled("handleMouseEnterEvent", true);

        // Not added to any root: the focus lists hold it, but filterList
        // drops it, so nothing routes.
        gui.handle_key_event(left_down(INSIDE));
        gui.mouse_cursor().position = INSIDE;
        gui.bump_frame();
        gui.handle_mouse_event(move_event(INSIDE));
        assert!(routing_log(&log).is_empty());

        // Added: routing resumes.
        gui.add_root(comp.clone());
        gui.handle_key_event(left_down(INSIDE));
        gui.bump_frame();
        gui.handle_mouse_event(move_event(INSIDE));
        assert_eq!(names(&routing_log(&log)).len(), 2);

        // Removed again: stale focus-list entries fall out of every route,
        // but the pending cross focus still fires one leave for the vanished
        // over-component (the reference holds it in a WeakPyPtr until the
        // next tick replaces it).
        gui.remove_root(&comp);
        let more = routing_log(&log).len();
        gui.handle_key_event(left_down(INSIDE));
        gui.bump_frame();
        gui.handle_mouse_event(move_event(INSIDE));
        assert_eq!(routing_log(&log).len(), more + 1);
        assert_eq!(
            names(&routing_log(&log)).last().unwrap().1,
            "handleMouseLeaveEvent"
        );
        // And nothing further ticks.
        gui.bump_frame();
        gui.handle_mouse_event(move_event(INSIDE));
        assert_eq!(routing_log(&log).len(), more + 1);
    }

    #[test]
    fn re_sort_orders_roots_by_depth_and_draw_order() {
        let mut gui = SimpleGui::new();

        let a = SimpleGuiComponent::new("a");
        a.borrow_mut().set_position(Vector3 {
            x: 0.4,
            y: 0.4,
            z: 0.5,
        });
        let b = SimpleGuiComponent::new("b");
        b.borrow_mut().set_position(Vector3 {
            x: 0.4,
            y: 0.4,
            z: 0.1,
        });
        let c = SimpleGuiComponent::new("c");
        c.borrow_mut().set_position(Vector3 {
            x: 0.4,
            y: 0.4,
            z: 0.9,
        });
        gui.add_root(a.clone());
        gui.add_root(b.clone());
        gui.add_root(c.clone());

        // depth_compare_gui_components: higher z first.
        let order: Vec<String> = gui
            .roots()
            .iter()
            .map(|c| c.borrow().name().to_string())
            .collect();
        assert_eq!(order, vec!["c", "a", "b"]);
        // recalcDrawOrders: increasing draw order in that same order.
        assert_eq!(c.borrow().draw_order(), 0);
        assert_eq!(a.borrow().draw_order(), 1);
        assert_eq!(b.borrow().draw_order(), 2);
    }

    #[test]
    fn children_draw_orders_follow_depth() {
        let mut gui = SimpleGui::new();
        let root = SimpleGuiComponent::new("root");
        gui.add_root(root.clone());

        let shallow = SimpleGuiComponent::new("shallow");
        shallow.borrow_mut().set_position(Vector3 {
            x: 0.4,
            y: 0.4,
            z: 0.3,
        });
        let deep = SimpleGuiComponent::new("deep");
        deep.borrow_mut().set_position(Vector3 {
            x: 0.4,
            y: 0.4,
            z: 0.7,
        });
        // Insertion order shallow-then-deep; draw order must follow z desc.
        gui.attach_child(&root, "shallow", shallow.clone());
        gui.attach_child(&root, "deep", deep.clone());

        assert_eq!(root.borrow().draw_order(), 0);
        assert_eq!(deep.borrow().draw_order(), 1);
        assert_eq!(shallow.borrow().draw_order(), 2);
    }

    #[test]
    fn add_root_ignores_duplicates_and_remove_respects_allow_delete() {
        let mut gui = SimpleGui::new();
        let comp = SimpleGuiComponent::new("a");
        gui.add_root(comp.clone());
        gui.add_root(comp.clone());
        assert_eq!(gui.roots().len(), 1);

        comp.borrow_mut().set_allow_delete(false);
        gui.remove_root(&comp);
        assert_eq!(gui.roots().len(), 1);

        comp.borrow_mut().set_allow_delete(true);
        gui.remove_root(&comp);
        assert_eq!(gui.roots().len(), 0);

        // clear drops the roots; the focus lists are deliberately untouched
        // (the reference keeps them too; filterList neutralises the stale
        // entries at routing time).
        let other = SimpleGuiComponent::new("b");
        gui.add_root(other.clone());
        gui.set_focus_flag(&other, FocusKind::MouseButton, true);
        gui.clear();
        assert!(gui.roots().is_empty());
    }

    #[test]
    fn the_hit_quad_and_its_clip_region_gate_the_hit() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let comp = scripted_component(&log, "a", POS, SIZE);

        // Interior points hit.
        assert!(comp.borrow().hit_test(None, INSIDE));
        assert!(comp.borrow().hit_test(None, Vector2 { x: 0.41, y: 0.39 }));
        // The sign test is boundary-EXCLUSIVE: an exact corner makes one
        // cross product zero, and `sig1 * sig2 <= 0` rejects the point.
        assert!(!comp.borrow().hit_test(None, Vector2 { x: 0.4, y: 0.4 }));
        assert!(!comp.borrow().hit_test(None, Vector2 { x: 0.6, y: 0.2 }));
        // Just outside each edge fails too.
        assert!(!comp.borrow().hit_test(None, OUTSIDE));
        assert!(!comp.borrow().hit_test(None, Vector2 { x: 0.61, y: 0.3 }));
        assert!(!comp.borrow().hit_test(None, Vector2 { x: 0.5, y: 0.19 }));

        // A component clip region narrows the quad.
        comp.borrow_mut().set_clip_region(Some(ClipRect {
            left: 0.5,
            top: 0.4,
            right: 0.6,
            bottom: 0.3,
        }));
        assert!(comp.borrow().hit_test(None, Vector2 { x: 0.55, y: 0.35 }));
        assert!(!comp.borrow().hit_test(None, Vector2 { x: 0.45, y: 0.35 }));
    }

    #[test]
    fn the_clip_stack_intersects_and_fails_on_empty_regions() {
        let mut gui = SimpleGui::new();
        let point = Vector2 { x: 0.5, y: 0.5 };

        // The constructor seeds the full-screen region (-1, 1, 1, -1).
        assert_eq!(gui.clip_stack.len(), 1);
        assert!(gui.is_point_in_clip_region(point));

        let first = ClipRect {
            left: 0.0,
            top: 0.8,
            right: 0.8,
            bottom: 0.0,
        };
        assert!(gui.push_clip_region(first));
        assert!(gui.is_point_in_clip_region(point));

        // A nested push intersects with the region below.
        let second = ClipRect {
            left: 0.4,
            top: 0.6,
            right: 1.0,
            bottom: -0.2,
        };
        assert!(gui.push_clip_region(second));
        assert!(gui.is_point_in_clip_region(point));
        assert!(!gui.is_point_in_clip_region(Vector2 { x: 0.1, y: 0.5 }));
        gui.pop_clip_region();

        // A push disjoint from the SCREEN region fails the validity check
        // and pops itself: the stack keeps the seed.
        let offscreen = ClipRect {
            left: -3.0,
            top: 3.0,
            right: -2.0,
            bottom: 2.0,
        };
        assert!(!gui.push_clip_region(offscreen));
        assert_eq!(gui.clip_stack.len(), 2);
        assert!(gui.is_point_in_clip_region(point));

        // Popping down past the seed leaves an empty stack, which answers
        // true.
        gui.pop_clip_region();
        gui.pop_clip_region();
        assert!(gui.clip_stack.is_empty());
        assert!(gui.is_point_in_clip_region(point));
        // And popping empty is a no-op.
        gui.pop_clip_region();
        assert!(gui.clip_stack.is_empty());

        // Pushing onto an emptied stack takes the region verbatim: there
        // is nothing below to intersect with.
        assert!(gui.push_clip_region(first));
        assert_eq!(gui.clip_stack.len(), 1);
        assert_eq!(gui.clip_stack[0], first);
    }

    #[test]
    fn the_clip_stack_tops_the_hit_test() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::MouseButton, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleMouseButtonEvent", true);

        // A clip region over the other half of the screen blanks the hit:
        // the button focus route finds nothing, and the click capture too.
        gui.push_clip_region(ClipRect {
            left: 0.0,
            top: 0.1,
            right: 0.3,
            bottom: 0.0,
        });
        gui.handle_key_event(left_down(INSIDE));
        gui.handle_key_event(left_up(INSIDE));
        assert!(routing_log(&log).is_empty());
        gui.pop_clip_region();

        gui.handle_key_event(left_down(INSIDE));
        gui.handle_key_event(left_up(INSIDE));
        assert_eq!(names(&routing_log(&log)).len(), 3);
    }

    #[test]
    fn the_key_event_predicates_match_the_reference() {
        let pos = Vector2::default();
        let down = left_down(pos);
        assert!(down.is_key_down());
        assert!(!down.is_key_up());
        assert!(down.is_mouse_button());

        let up = left_up(pos);
        assert!(up.is_key_up());
        assert!(!up.is_key_down());

        for key in [KEY_RIGHTMOUSE, KEY_MIDDLEMOUSE] {
            assert!(
                KeyEvent {
                    key,
                    state: 0,
                    modifiers: 0,
                    cursor_position: pos
                }
                .is_mouse_button()
            );
        }
        assert!(
            !KeyEvent {
                key: 0x30,
                state: 0,
                modifiers: 0,
                cursor_position: pos
            }
            .is_mouse_button()
        );

        // MouseEvent::fill is the enter/leave detection's zeroed event.
        let filled = MouseEvent::fill(0, 0, 0, pos);
        assert_eq!(
            filled,
            MouseEvent {
                dx: 0,
                dy: 0,
                dz: 0,
                cursor_position: pos
            }
        );
        assert_eq!(
            filled.cursor_position - Vector2 { x: 1.0, y: 2.0 },
            Vector2 { x: -1.0, y: -2.0 }
        );
        assert_eq!(Vector2 { x: 3.0, y: 4.0 }.length_squared(), 25.0);
    }

    #[test]
    fn draw_order_range_exhaustion_falls_back_to_a_full_recalc() {
        // calcDrawOrderRecursively's guards (nextDrawOrder > 0) are only
        // reachable when a bounded range is imposed; drive them directly.
        let mut gui = SimpleGui::new();
        let root = SimpleGuiComponent::new("root");
        gui.add_root(root.clone());
        let a = SimpleGuiComponent::new("a");
        gui.attach_child(&root, "a", a.clone());
        let b = SimpleGuiComponent::new("b");
        gui.attach_child(&root, "b", b.clone());

        // Three components need three slots; a two-slot range fails.
        assert_eq!(super::calc_draw_order_recursively(&root, 0, 2), 0);
        // A three-slot range fits exactly.
        assert_eq!(super::calc_draw_order_recursively(&root, 0, 3), 3);
        // An unbounded range (the recalc path) returns the next free order.
        assert_eq!(super::calc_draw_order_recursively(&root, 0, 0), 3);
        assert_eq!(a.borrow().draw_order(), 1);
        assert_eq!(b.borrow().draw_order(), 2);
    }

    #[test]
    fn set_drag_distance_resquares_the_threshold() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);
        let comp = scripted_component(&log, "a", POS, SIZE);
        gui.add_root(comp.clone());
        gui.set_focus_flag(&comp, FocusKind::Drag, true);
        let script = comp.borrow().script().unwrap().clone();
        script.set_handled("handleDragStartEvent", true);

        gui.handle_key_event(left_down(INSIDE));
        // A move 0.01 away with the distance at 0.005^2: crossed.
        gui.set_drag_distance(0.005);
        let near = Vector2 {
            x: INSIDE.x + 0.01,
            y: INSIDE.y,
        };
        assert!(gui.handle_mouse_event(move_event(near)));
        assert!(
            names(&routing_log(&log))
                .iter()
                .any(|(_, cb)| cb == "handleDragStartEvent")
        );
    }

    #[test]
    fn default_drag_distance_is_the_reference_constant_squared() {
        // c_defaultDrgaDistance = 0.002 -> 4e-6.
        let gui = SimpleGui::new();
        // The squared default sits between a nearby move and a far move.
        let near = 0.0019 * 0.0019;
        let far = 0.0021 * 0.0021;
        assert!(near < crate::simple_gui::DEFAULT_DRAG_DISTANCE * DEFAULT_DRAG_DISTANCE);
        assert!(far > DEFAULT_DRAG_DISTANCE * DEFAULT_DRAG_DISTANCE);
        let _ = gui;
    }

    #[test]
    fn an_invisible_component_declines_the_key_mouse_and_axis_events() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let _gui = gui_with_cursor_at(INSIDE);

        let comp = scripted_component(&log, "a", POS, SIZE);
        comp.borrow_mut().set_visible(false);

        let key = KeyEvent {
            key: 0x30,
            state: 0,
            modifiers: 0,
            cursor_position: INSIDE,
        };
        assert!(!comp.borrow().handle_key_event(key));
        assert!(!comp.borrow().handle_mouse_event(None, move_event(INSIDE)));
        assert!(!comp.borrow().handle_axis_event(AxisEvent {
            axis: 0,
            value: 0.5,
            dtime: 0.016
        }));
        // The scripts never saw any of it.
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn the_handler_gates_and_scriptless_components_decline() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        // Without the mouse-button focus flag the button event declines
        // before any script is consulted.
        let plain = scripted_component(&log, "plain", POS, SIZE);
        assert!(!plain.borrow().handle_mouse_button_event(left_down(INSIDE)));

        // With the flag but no script bound, every handler declines.
        let quiet = SimpleGuiComponent::new("quiet");
        quiet.borrow_mut().set_position(POS);
        quiet.borrow_mut().set_size(SIZE.0, SIZE.1);
        gui.set_focus_flag(&quiet, FocusKind::MouseButton, true);
        assert!(!quiet.borrow().handle_mouse_button_event(left_down(INSIDE)));
        assert!(!quiet.borrow().handle_mouse_enter_event(move_event(INSIDE)));
        assert!(!quiet.borrow().handle_mouse_click_event(left_up(INSIDE)));
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn children_loops_continue_past_declining_children() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        let mut gui = gui_with_cursor_at(INSIDE);

        let parent = scripted_component(&log, "parent", POS, SIZE);
        gui.add_root(parent.clone());
        gui.set_focus_flag(&parent, FocusKind::Focus, true);
        gui.set_focus_flag(&parent, FocusKind::Move, true);
        let parent_script = parent.borrow().script().unwrap().clone();
        parent_script.set_handled("handleKeyEvent", true);
        parent_script.set_handled("handleMouseEvent", true);
        parent_script.set_handled("handleAxisEvent", true);

        // The child carries the flags but declines everything: each walk
        // continues past it to the parent.
        let child = scripted_component(&log, "child", POS, SIZE);
        gui.attach_child(&parent, "child", child.clone());
        gui.set_focus_flag(&child, FocusKind::Focus, true);
        gui.set_focus_flag(&child, FocusKind::Move, true);

        let key = KeyEvent {
            key: 0x30,
            state: 0,
            modifiers: 0,
            cursor_position: INSIDE,
        };
        assert!(gui.handle_key_event(key));
        assert!(gui.handle_mouse_event(move_event(INSIDE)));
        assert!(gui.handle_axis_event(AxisEvent {
            axis: 0,
            value: 0.5,
            dtime: 0.016
        }));

        // The child's scripts ran and declined; every walk continued past
        // it — the parent's handlers ran too.
        let calls = names(&routing_log(&log));
        for callback in ["handleKeyEvent", "handleMouseEvent", "handleAxisEvent"] {
            assert!(calls.contains(&("child".to_string(), callback.to_string())));
            assert!(calls.contains(&("parent".to_string(), callback.to_string())));
        }

        // A child that accepts stops the walk before the parent.
        let child_script = child.borrow().script().unwrap().clone();
        child_script.set_handled("handleAxisEvent", true);
        log.borrow_mut().clear();
        assert!(gui.handle_axis_event(AxisEvent {
            axis: 0,
            value: 0.5,
            dtime: 0.016
        }));
        assert_eq!(
            names(&routing_log(&log)),
            vec![("child".to_string(), "handleAxisEvent".to_string())]
        );
    }

    #[test]
    fn default_gui_and_child_lookup_match_the_accessors() {
        let log: ScriptLog = Rc::new(RefCell::new(Vec::new()));
        // Default is the plain constructor (the clip seed included).
        let mut gui = SimpleGui::default();
        assert_eq!(gui.clip_stack.len(), 1);

        let parent = scripted_component(&log, "parent", POS, SIZE);
        gui.add_root(parent.clone());
        assert_eq!(parent.borrow().script().unwrap().name(), "parent");

        let child = scripted_component(&log, "kid", POS, SIZE);
        gui.attach_child(&parent, "kid", child.clone());
        assert!(Rc::ptr_eq(&parent.borrow().child("kid").unwrap(), &child));
        assert!(parent.borrow().child("nobody").is_none());
    }

    #[test]
    fn draw_order_grandchildren_trip_both_zero_guards() {
        let mut gui = SimpleGui::new();

        let root = SimpleGuiComponent::new("root");
        gui.add_root(root.clone());
        let a = SimpleGuiComponent::new("a");
        gui.attach_child(&root, "a", a.clone());
        let deep = SimpleGuiComponent::new("deep");
        gui.attach_child(&a, "deep", deep.clone());
        let b = SimpleGuiComponent::new("b");
        gui.attach_child(&root, "b", b.clone());

        // root -> a -> deep eats slots 1 and 2; sibling "b" finds the
        // range exhausted inside the loop: the in-loop guard zeroes it.
        assert_eq!(super::calc_draw_order_recursively(&root, 0, 3), 0);
        assert_eq!(a.borrow().draw_order(), 1);
        assert_eq!(deep.borrow().draw_order(), 2);

        // With "a" over-subscribed, its own capacity check fails first and
        // the zero propagates through the child-returned-zero guard. (The
        // root's range must still fit, or its entry guard would cut in.)
        let g1 = SimpleGuiComponent::new("g1");
        gui.attach_child(&a, "g1", g1.clone());
        let g2 = SimpleGuiComponent::new("g2");
        gui.attach_child(&a, "g2", g2.clone());
        assert_eq!(super::calc_draw_order_recursively(&root, 0, 3), 0);
        // Unbounded, the same tree lays out 1..5 and answers 6.
        assert_eq!(super::calc_draw_order_recursively(&root, 0, 0), 6);
        assert_eq!(b.borrow().draw_order(), 5);
    }
}
