//! BW-11-05-05 web-page embedding: the web_render library's
//! `AwesomiumProvider` workflow and the `WebApp` main-loop task.
//!
//! The reference splits the feature over three files:
//!
//! * `lib/web_render/awesomium_provider.hpp/.cpp` — `PyAwesomiumProvider`
//!   (a `PyTextureProvider` wrapping an Awesomium `WebView`), the
//!   `BWResourceInterceptor` that serves `file://` URLs from the resource
//!   system, the `AwesomiumTexture` D3D dynamic texture, the lazy
//!   `checkWebCore` DLL load, and the static init/fini/tick;
//! * `client/web_app.cpp` — the `WebApp` task registering "Web/App";
//! * `lib/web_render/CMakeLists.txt` — always builds the library, defines
//!   `BW_AWESOMIUM_SUPPORT=0` unless the SDK is present.
//!
//! The Awesomium SDK itself (`third_party/awesomium`, `awesomium.dll`,
//! `awesomium_process.exe`) does **not** ship in the reference tree, and
//! `ENABLE_AWESOMIUM` is hard-wired 0 on `_WIN64`
//! (`awesomium_provider.hpp:5-9`) — see [`WEB_RENDER_TARGETS`] for the
//! missing-SDK registry. The acceptance is that the original's *usable
//! workflow* has a counterpart: factory `(width, height)` → lazy WebCore
//! load (`awesomium.dll` next to the exe, else the `awesomium/` res path)
//! → `CreateWebView` + listener + dynamic texture → navigation/injection →
//! per-frame `Update` + texture copy → `cleanup`. The absent SDK's side of
//! every call is the observable `WebOp`/`WebCoreOp` ledger.
//!
//! Reference quirks preserved verbatim: the constructor's "Awesomium not
//! initialisd." typo, the double `is_system_key = false` assignment with
//! its TODO, `extensionToMime`'s "Sensible default?" fallback, `toVKey`'s
//! linear reverse scan comment, and the doc-comment/script-call split (the
//! header documents 6 script callbacks; the dispatch actually fires 9).

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::{Rc, Weak};

/// `AWESOMIUM_DLL` (`awesomium_provider.cpp:32`).
pub const AWESOMIUM_DLL: &str = "awesomium.dll";
/// `AWESOMIUM_RES_PATH` (`awesomium_provider.cpp:33`).
pub const AWESOMIUM_RES_PATH: &str = "awesomium/";
/// The child-process and log file names `checkWebCoreImpl` builds.
pub const AWESOMIUM_PROCESS: &str = "awesomium_process.exe";
pub const AWESOMIUM_LOG: &str = "awesomium.log";
/// `BWResource::appDirectory()` — the "next to the client executable"
/// probe path (no trailing slash; `checkWebCoreImpl` appends `\\`).
pub const APP_DIRECTORY: &str = "game";

/// `ENABLE_AWESOMIUM` (`awesomium_provider.hpp:5-9`): hard-wired 0 on
/// `_WIN64`, otherwise `BW_AWESOMIUM_SUPPORT`.
pub const ENABLE_AWESOMIUM_ON_WIN64: bool = false;
/// The library's CMake default when the SDK is absent
/// (`web_render/CMakeLists.txt` defines `BW_AWESOMIUM_SUPPORT=0`).
pub const BW_AWESOMIUM_SUPPORT_DEFAULT: bool = false;

/// Whether the provider statics would compile in at all: the whole
/// `awesomium_provider.cpp` body sits inside `#if ENABLE_AWESOMIUM`.
pub fn awesomium_gate_open() -> bool {
    !ENABLE_AWESOMIUM_ON_WIN64 && BW_AWESOMIUM_SUPPORT_DEFAULT
}

/// The web-render implementation registry — the 缺 SDK ledger for this
/// item. The library body is fully compiled out unless the SDK ships.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WebRenderTarget {
    pub name: &'static str,
    /// The files `checkWebCore`/`checkWebCoreImpl` look for next to the
    /// executable or in the resource path.
    pub sdk_files: &'static [&'static str],
    /// Whether the SDK source ships in the reference tree.
    pub sdk_source_available: bool,
    /// The gating condition that actually enables the provider.
    pub enabled_when: &'static str,
}

pub const WEB_RENDER_TARGETS: &[WebRenderTarget] = &[WebRenderTarget {
    name: "awesomium",
    sdk_files: &[AWESOMIUM_DLL, AWESOMIUM_PROCESS],
    sdk_source_available: false,
    enabled_when: "32-bit windows build with BW_AWESOMIUM_SUPPORT=1 and the DLL beside the exe",
}];

// ---------------------------------------------------------------------------
// Key input face (lib/input + the absent SDK's WebKeyboardEvent)
// ---------------------------------------------------------------------------

/// The key codes `injectKeyEvent` branches on. A compact subset of the
/// reference `KeyCode::Key` enum (`lib/input/keycode.hpp`) — the ones the
/// workflow exercises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyCode {
    LeftMouse,
    RightMouse,
    MiddleMouse,
    Return,
    Escape,
    Space,
    KeyA,
    KeyB,
    Unknown,
}

/// `MouseButton` values the WebView injection API takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButtonKind {
    Left,
    Right,
    Middle,
}

/// `convertMouseButton` (`awesomium_provider.cpp:110`): three mouse codes
/// map to their button; anything else is an `MF_ASSERT` (the port panics
/// with the same condition) before the defensive `kMouseButton_Left`.
pub fn convert_mouse_button(key: KeyCode) -> MouseButtonKind {
    match key {
        KeyCode::LeftMouse => MouseButtonKind::Left,
        KeyCode::RightMouse => MouseButtonKind::Right,
        KeyCode::MiddleMouse => MouseButtonKind::Middle,
        _ => panic!("convertMouseButton: non-mouse code given"),
    }
}

/// The modifier mask `WebKeyboardEvent::modifiers` carries. The three
/// `kMod*Key` numerics live in the absent SDK; the OR order is the
/// reference's.
pub const K_MOD_ALT_KEY: u32 = 1;
pub const K_MOD_CONTROL_KEY: u32 = 2;
pub const K_MOD_SHIFT_KEY: u32 = 4;

/// The script-visible key event (`KeyEvent` in `lib/input`).
#[derive(Clone, Debug, Default)]
pub struct KeyEvent {
    pub key: Option<KeyCode>,
    /// `isKeyDown()`.
    pub key_down: bool,
    /// `isMouseButton()`.
    pub mouse_button: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    /// `utf16Char()` — empty means no character payload.
    pub utf16: Vec<u16>,
}

impl KeyEvent {
    /// A key-down with no modifiers.
    pub fn key_down(key: KeyCode) -> KeyEvent {
        KeyEvent {
            key: Some(key),
            key_down: true,
            ..KeyEvent::default()
        }
    }

    /// A key-up with no modifiers.
    pub fn key_up(key: KeyCode) -> KeyEvent {
        KeyEvent {
            key: Some(key),
            key_down: false,
            ..KeyEvent::default()
        }
    }

    /// A mouse-button press or release.
    pub fn mouse(key: KeyCode, down: bool) -> KeyEvent {
        KeyEvent {
            key: Some(key),
            key_down: down,
            mouse_button: true,
            ..KeyEvent::default()
        }
    }
}

/// `convertModifiers` (`awesomium_provider.cpp:131`): alt, ctrl and shift
/// each OR their `kMod*Key` bit.
pub fn convert_modifiers(event: &KeyEvent) -> u32 {
    let mut m = 0;
    if event.alt {
        m |= K_MOD_ALT_KEY;
    }
    if event.ctrl {
        m |= K_MOD_CONTROL_KEY;
    }
    if event.shift {
        m |= K_MOD_SHIFT_KEY;
    }
    m
}

/// `VKMap::toVKey` (`lib/input/vk_map.cpp:297`): a linear reverse scan of
/// the VK→KeyCode table, first match wins, unknown keys give 0. This port
/// carries the compact slice of that table the workflow needs; the
/// reference's own comment is kept:
///
/// > For what we need this for, this is good enough. Would be nicer to
/// > have a proper reverse LUT however.
pub fn vk_map_to_vkey(key: KeyCode) -> u16 {
    const VK_LBUTTON: u16 = 0x01;
    const VK_RBUTTON: u16 = 0x02;
    const VK_MBUTTON: u16 = 0x04;
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_SPACE: u16 = 0x20;
    const VK_KEY_A: u16 = 0x41;
    const VK_KEY_B: u16 = 0x42;
    match key {
        KeyCode::LeftMouse => VK_LBUTTON,
        KeyCode::RightMouse => VK_RBUTTON,
        KeyCode::MiddleMouse => VK_MBUTTON,
        KeyCode::Return => VK_RETURN,
        KeyCode::Escape => VK_ESCAPE,
        KeyCode::Space => VK_SPACE,
        KeyCode::KeyA => VK_KEY_A,
        KeyCode::KeyB => VK_KEY_B,
        KeyCode::Unknown => 0,
    }
}

/// Stand-in for the SDK's `GetKeyIdentifierFromVirtualKeyCode` (the
/// Awesomium SDK is not in the tree): a deterministic hex identifier.
pub fn key_identifier_from_vk(vk: u16) -> String {
    format!("VKey_{:#06X}", vk)
}

/// Stand-in for Win32 `MapVirtualKey(vk, MAPVK_VK_TO_VSC)` — the exact
/// scancode table lives in the OS, so the port keeps the low byte.
pub fn map_virtual_key_vk_to_vsc(vk: u16) -> u16 {
    vk & 0xFF
}

/// `WebKeyboardEvent` as far as the injection builds it.
#[derive(Clone, PartialEq, Debug)]
pub enum WebKeyEventType {
    KeyDown,
    KeyUp,
    /// The separate `WM_CHAR` event inserted when the key carries a
    /// character (`awesomium_provider.cpp:879-886`).
    Char,
}

#[derive(Clone, PartialEq, Debug)]
pub struct WebKeyboardEvent {
    pub kind: WebKeyEventType,
    pub is_system_key: bool,
    pub virtual_key_code: u16,
    pub modifiers: u32,
    pub key_identifier: String,
    pub native_key_code: u16,
    /// The `MAKEWPARAM(chr[0], chr[1])` payload of a `Char` event.
    pub char_text: String,
}

// ---------------------------------------------------------------------------
// The observable SDK ledger
// ---------------------------------------------------------------------------

/// One `WebView` API call with its arguments.
#[derive(Clone, PartialEq, Debug)]
pub enum WebOp {
    SetViewListener,
    SetLoadListener,
    LoadURL {
        url: String,
    },
    Stop,
    Reload {
        ignore_cache: bool,
    },
    GoBack,
    GoForward,
    Focus,
    Unfocus,
    Resize {
        width: i32,
        height: i32,
    },
    InjectMouseDown {
        button: MouseButtonKind,
    },
    InjectMouseUp {
        button: MouseButtonKind,
    },
    InjectKeyboardEvent {
        kind: WebKeyEventType,
        is_system_key: bool,
        virtual_key_code: u16,
        modifiers: u32,
        key_identifier: String,
        native_key_code: u16,
        char_text: String,
    },
    InjectMouseMove {
        x: i32,
        y: i32,
    },
    InjectMouseWheel {
        dz: i32,
        dw: i32,
    },
    ExecuteJavascript {
        script: String,
        frame_xpath: String,
    },
    ExecuteJavascriptWithResult {
        script: String,
        frame_xpath: String,
    },
    Destroy,
}

/// One `WebCore`/DLL-level call.
#[derive(Clone, PartialEq, Debug)]
pub enum WebCoreOp {
    LoadLibrary {
        path: String,
    },
    FreeLibrary,
    Initialize {
        child_process_path: String,
        log_path: String,
        plugin_path: String,
        package_path: String,
    },
    SetResourceInterceptor {
        installed: bool,
    },
    Update,
    Shutdown,
}

/// The `BitmapSurface` state the texture copies from.
#[derive(Clone, Debug)]
pub struct SurfaceState {
    pub width: i32,
    pub height: i32,
    pub dirty: bool,
    pub buffer: Vec<u8>,
}

impl SurfaceState {
    pub fn new(width: i32, height: i32) -> SurfaceState {
        SurfaceState {
            width,
            height,
            dirty: false,
            buffer: vec![0; (width * height * 4) as usize],
        }
    }
}

/// The `WebView` state the provider drives. The absent SDK's behaviour is
/// modelled just deep enough to observe the provider's workflow: the URL
/// and loading flag respond to `LoadURL`/`Stop`, `ExecuteJavascript…`
/// consumes scripted results, and the surface dirty flag drives the
/// texture copy gate.
pub struct WebViewState {
    pub id: usize,
    pub width: i32,
    pub height: i32,
    pub url: String,
    pub loading: bool,
    pub focused: bool,
    pub destroyed: bool,
    pub surface: SurfaceState,
    pub ops: Vec<WebOp>,
    /// Queued strings returned by `ExecuteJavascriptWithResult`.
    pub js_results: VecDeque<String>,
    /// Set to make `CreateWebView` fail (max web views exhausted etc.).
    pub view_listener_installed: bool,
    pub load_listener_installed: bool,
}

impl WebViewState {
    fn new(id: usize, width: i32, height: i32) -> WebViewState {
        WebViewState {
            id,
            width,
            height,
            url: String::new(),
            loading: false,
            focused: false,
            destroyed: false,
            surface: SurfaceState::new(width, height),
            ops: Vec::new(),
            js_results: VecDeque::new(),
            view_listener_installed: false,
            load_listener_installed: false,
        }
    }

    /// The recorded WebView calls, in call order.
    pub fn ops(&self) -> &[WebOp] {
        &self.ops
    }
}

// ---------------------------------------------------------------------------
// The resource interceptor face
// ---------------------------------------------------------------------------

/// `extensionToMime` (`awesomium_provider.cpp:167`) — the table verbatim,
/// including the "Sensible default?" comment's fallback.
pub fn extension_to_mime(ext: &str) -> &'static str {
    // Not exhaustive, but should cover common cases.
    if ext == "html" || ext == "htm" || ext == "htmls" {
        "text/html"
    } else if ext == "css" {
        "text/css"
    } else if ext == "xml" || ext == "xhtml" {
        "text/xml"
    } else if ext == "js" {
        "text/javascript"
    } else if ext == "jpeg" || ext == "jpg" {
        "image/jpeg"
    } else if ext == "gif" {
        "image/gif"
    } else if ext == "png" {
        "image/png"
    } else if ext == "swf" {
        "application/x-shockwave-flash"
    } else {
        // Sensible default?
        "text/plain"
    }
}

/// `BWResource::getExtension` face: the text after the last dot.
pub fn resource_extension(path: &str) -> String {
    match path.rfind('.') {
        Some(idx) => path[idx + 1..].to_string(),
        None => String::new(),
    }
}

/// A `ResourceRequest` as the interceptor inspects it.
#[derive(Clone, Debug)]
pub struct ResourceRequest {
    pub scheme: String,
    pub path: String,
}

/// The `ResourceResponse` the interceptor creates on a hit.
#[derive(Clone, PartialEq, Debug)]
pub struct ResourceResponse {
    pub data: Vec<u8>,
    pub mime: String,
}

/// The resource-system face the interceptor reads through
/// (`BWResource::instance().fileSystem()->readFile`).
#[derive(Default)]
pub struct ResourceSystem {
    files: BTreeMap<String, Vec<u8>>,
}

impl ResourceSystem {
    pub fn new() -> ResourceSystem {
        ResourceSystem::default()
    }

    pub fn add_file(&mut self, path: &str, bytes: &[u8]) {
        self.files.insert(path.to_string(), bytes.to_vec());
    }

    /// `readFile`: `None` when the path is not in the system.
    pub fn read_file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|bytes| bytes.as_slice())
    }
}

/// `BWResourceInterceptor::OnRequest` (`awesomium_provider.cpp:217`):
/// only `file` scheme URLs are served from the resource system; a failed
/// read and a foreign scheme both yield `NULL` (the SDK falls back to its
/// own loading).
pub fn resource_interceptor_on_request(
    system: &ResourceSystem,
    request: &ResourceRequest,
) -> Option<ResourceResponse> {
    if request.scheme != "file" {
        return None;
    }
    let blob = system.read_file(&request.path)?;
    let mime = extension_to_mime(&resource_extension(&request.path));
    Some(ResourceResponse {
        data: blob.to_vec(),
        mime: mime.to_string(),
    })
}

// ---------------------------------------------------------------------------
// AwesomiumTexture
// ---------------------------------------------------------------------------

/// `AwesomiumTexture` (`awesomium_provider.cpp:253`): a D3D dynamic
/// A8R8G8B8 texture the web view surface is blitted into.
///
/// `createUnmanagedObjects` fills every byte with 0xFF (the
/// `memset(lockedRect.pBits, 0xFFFFFFFF, ...)` white wash), `resize` is
/// delete-then-create, and `copyWebView` only locks when the texture
/// exists, the surface exists, and the surface is dirty or a copy was
/// forced.
pub struct AwesomiumTexture {
    width: i32,
    height: i32,
    force_update: bool,
    /// Set to model the D3D `LockRect` failing (`hr != S_OK`).
    pub lock_fails: bool,
    /// The locked texture bytes (`None` while the unmanaged object is
    /// deleted).
    pub pixels: Option<Vec<u8>>,
    /// The `ERROR_MSG` lines the texture emits.
    pub logs: Vec<String>,
}

impl AwesomiumTexture {
    pub fn new(width: i32, height: i32) -> AwesomiumTexture {
        let mut texture = AwesomiumTexture {
            width,
            height,
            force_update: true,
            lock_fails: false,
            pixels: None,
            logs: Vec::new(),
        };
        texture.create_unmanaged_objects();
        texture
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    /// `format()`: always A8R8G8B8, 4 bytes per pixel.
    pub fn bytes_per_pixel(&self) -> usize {
        4
    }

    /// `textureMemoryUsed()`: width × height × 4.
    pub fn texture_memory_used(&self) -> usize {
        (self.width * self.height) as usize * 4
    }

    /// `recreateForD3DExDevice()`: always true.
    pub fn recreate_for_d3d_ex_device(&self) -> bool {
        true
    }

    fn create_unmanaged_objects(&mut self) {
        // MF_ASSERT(!pTex_.hasComObject()) — a fresh texture never has one.
        self.pixels = Some(vec![0xFF; self.texture_memory_used()]);
        self.force_update = true;
    }

    fn delete_unmanaged_objects(&mut self) {
        self.pixels = None;
    }

    /// `resize`: delete the unmanaged object, change the size, recreate.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.delete_unmanaged_objects();
        self.width = width;
        self.height = height;
        self.create_unmanaged_objects();
    }

    /// Whether the next `copyWebView` would lock (the dirty-or-forced
    /// gate, without the copy).
    fn copy_wanted(&self, surface: Option<&SurfaceState>) -> bool {
        self.pixels.is_some() && surface.is_some() && (surface.unwrap().dirty || self.force_update)
    }

    /// `copyWebView` (`awesomium_provider.cpp:309`): the guarded surface
    /// blit. Returns whether a lock was attempted.
    pub fn copy_web_view(&mut self, view: &WebViewState) -> bool {
        let surface = if view.destroyed {
            None
        } else {
            Some(&view.surface)
        };
        if !self.copy_wanted(surface) {
            return false;
        }
        let surface = surface.unwrap();
        assert!(
            surface.width == self.width,
            "MF_ASSERT: pSurface->width() == width_"
        );
        assert!(
            surface.height == self.height,
            "MF_ASSERT: pSurface->height() == height_"
        );

        if self.lock_fails {
            self.logs.push(
                "AwesomiumTexture::copyWebView: failed to lock D3D texture (lock failed).\n"
                    .to_string(),
            );
        } else {
            self.pixels = Some(surface.buffer.clone());
        }
        // forceUpdate_ is cleared whether the lock succeeded or not.
        self.force_update = false;
        true
    }
}

// ---------------------------------------------------------------------------
// The script callback face (PyAwesomiumProvider::Listener)
// ---------------------------------------------------------------------------

/// One script argument: strings, the `(Lis)` int64 frame ids, bools and
/// the `(i)` cursor code.
#[derive(Clone, PartialEq, Debug)]
pub enum ScriptArg {
    Str(String),
    Int(i64),
    Bool(bool),
}

/// One dispatched script call: the callback name, its tuple, and the
/// `Script::call` error prefix.
#[derive(Clone, PartialEq, Debug)]
pub struct ScriptCall {
    pub callback: &'static str,
    pub args: Vec<ScriptArg>,
    pub prefix: &'static str,
}

/// `PyAwesomiumProvider::Listener` face: holds the script object and
/// dispatches the WebView events to it. The reference's header doc lists
/// six callbacks; the dispatch (`awesomium_provider.cpp:356-545`) fires
/// these nine — all gated on `if (pObject_)`.
pub struct WebListener {
    object: Option<String>,
    calls: Vec<ScriptCall>,
}

impl WebListener {
    fn new() -> WebListener {
        WebListener {
            object: None,
            calls: Vec::new(),
        }
    }

    /// `pObject` setter — any script object implementing any subset of the
    /// callbacks.
    pub fn set_object(&mut self, object: Option<&str>) {
        self.object = object.map(|name| name.to_string());
    }

    /// The script object currently attached (the `script` attribute).
    pub fn object(&self) -> Option<&str> {
        self.object.as_deref()
    }

    /// The dispatched calls, in dispatch order.
    pub fn calls(&self) -> &[ScriptCall] {
        &self.calls
    }

    fn dispatch(&mut self, callback: &'static str, prefix: &'static str, args: Vec<ScriptArg>) {
        // Script::call(PyObject_GetAttrString(...)) — the gate is the
        // object being set; a missing callback attribute would just fail
        // the attr lookup and be swallowed by Script::call.
        if self.object.is_none() {
            return;
        }
        self.calls.push(ScriptCall {
            callback,
            args,
            prefix,
        });
    }

    pub fn on_change_title(&mut self, title: &str) {
        self.dispatch(
            "onChangeTitle",
            "PyAwesomiumProvider::onChangeTitle: ",
            vec![ScriptArg::Str(title.to_string())],
        );
    }

    pub fn on_change_address_bar(&mut self, url: &str) {
        self.dispatch(
            "onChangeAddressBar",
            "PyAwesomiumProvider::onChangeAddressBar: ",
            vec![ScriptArg::Str(url.to_string())],
        );
    }

    pub fn on_change_tooltip(&mut self, tooltip: &str) {
        self.dispatch(
            "onChangeTooltip",
            "PyAwesomiumProvider::onChangeTooltip: ",
            vec![ScriptArg::Str(tooltip.to_string())],
        );
    }

    pub fn on_change_target_url(&mut self, url: &str) {
        self.dispatch(
            "onChangeTargetURL",
            "PyAwesomiumProvider::onChangeTargetURL: ",
            vec![ScriptArg::Str(url.to_string())],
        );
    }

    pub fn on_change_cursor(&mut self, cursor: i32) {
        self.dispatch(
            "onChangeCursor",
            "PyAwesomiumProvider::onChangeCursor: ",
            vec![ScriptArg::Int(cursor as i64)],
        );
    }

    pub fn on_begin_loading_frame(&mut self, frame_id: i64, is_main_frame: bool, url: &str) {
        self.dispatch(
            "onBeginLoadingFrame",
            "PyAwesomiumProvider::onBeginLoadingFrame: ",
            vec![
                ScriptArg::Int(frame_id),
                ScriptArg::Bool(is_main_frame),
                ScriptArg::Str(url.to_string()),
            ],
        );
    }

    pub fn on_fail_loading_frame(
        &mut self,
        frame_id: i64,
        is_main_frame: bool,
        error_code: i32,
        url: &str,
    ) {
        self.dispatch(
            "onFailLoadingFrame",
            "PyAwesomiumProvider::onFailLoadingFrame: ",
            vec![
                ScriptArg::Int(frame_id),
                ScriptArg::Bool(is_main_frame),
                ScriptArg::Int(error_code as i64),
                ScriptArg::Str(url.to_string()),
            ],
        );
    }

    pub fn on_finish_loading_frame(&mut self, frame_id: i64, is_main_frame: bool, url: &str) {
        self.dispatch(
            "onFinishLoadingFrame",
            "PyAwesomiumProvider::onFinishLoadingFrame: ",
            vec![
                ScriptArg::Int(frame_id),
                ScriptArg::Bool(is_main_frame),
                ScriptArg::Str(url.to_string()),
            ],
        );
    }

    pub fn on_document_ready(&mut self, url: &str) {
        self.dispatch(
            "onDocumentReady",
            "PyAwesomiumProvider::onDocumentReady: ",
            vec![ScriptArg::Str(url.to_string())],
        );
    }
}

// ---------------------------------------------------------------------------
// WebCoreContext — the WebCore/DLL statics and the absent SDK's runtime
// ---------------------------------------------------------------------------

/// Every C++ static of `PyAwesomiumProvider` plus the SDK runtime face:
/// `s_webCoreInitialised_`, `s_awesomiumModule_`, `pResourceInterceptor_`,
/// `s_providers_`, and the `WebCore::Initialize/instance/Update/Shutdown`
/// calls themselves.
pub struct WebCoreContext {
    pub initialised: bool,
    pub module_loaded: bool,
    /// The path `LoadLibraryA` succeeded on.
    pub module_path: Option<String>,
    pub interceptor_installed: bool,
    /// Injected failure: `LoadLibraryA` cannot load the DLL.
    pub load_library_fails: bool,
    /// Injected failure: `WebCore::Initialize` rejects the config.
    pub initialize_fails: bool,
    /// Injected failure: `CreateWebView` returns NULL.
    pub create_view_fails: bool,
    /// The `awesomium.dll` probe next to the executable.
    pub dll_beside_exe: bool,
    /// The `awesomium/awesomium.dll` probe in the resource path.
    pub dll_in_res_path: bool,
    /// What `BWResolver::resolveFilename` returns for the res-path DLL
    /// (a path without a backslash drives the `npos` error branch).
    pub res_dll_resolved: Option<String>,
    /// The live web views, in creation order.
    pub views: Vec<Rc<RefCell<WebViewState>>>,
    /// The live providers (`s_providers_`); dead entries are pruned like
    /// the C++ destructor's `erase`.
    pub providers: Vec<Weak<RefCell<PyAwesomiumProvider>>>,
    /// The resource system the interceptor serves `file://` URLs from.
    pub resources: Rc<ResourceSystem>,
    pub core_ops: Vec<WebCoreOp>,
    /// The `ERROR_MSG`/`INFO_MSG`/`CRITICAL_MSG` lines.
    pub logs: Vec<String>,
    /// The version string `WebCore::instance()->version_string()` returns.
    pub version: &'static str,
}

impl WebCoreContext {
    pub fn new() -> WebCoreContext {
        WebCoreContext {
            initialised: false,
            module_loaded: false,
            module_path: None,
            interceptor_installed: false,
            load_library_fails: false,
            initialize_fails: false,
            create_view_fails: false,
            dll_beside_exe: false,
            dll_in_res_path: false,
            res_dll_resolved: None,
            views: Vec::new(),
            providers: Vec::new(),
            resources: Rc::new(ResourceSystem::new()),
            core_ops: Vec::new(),
            logs: Vec::new(),
            version: "1.7.0.0",
        }
    }

    /// `WebCore::instance()` face: the live core while initialised.
    pub fn instance(&self) -> bool {
        self.initialised
    }

    /// The live (non-destroyed, undropped) providers.
    pub fn live_providers(&mut self) -> Vec<Rc<RefCell<PyAwesomiumProvider>>> {
        self.providers.retain(|weak| weak.upgrade().is_some());
        self.providers
            .iter()
            .filter_map(|weak| weak.upgrade())
            .collect()
    }

    /// The `ERROR_MSG`/`INFO_MSG`/`CRITICAL_MSG` transcript.
    pub fn logs(&self) -> &[String] {
        &self.logs
    }
}

impl Default for WebCoreContext {
    fn default() -> WebCoreContext {
        WebCoreContext::new()
    }
}

/// `PyAwesomiumProvider::checkWebCore` (`awesomium_provider.cpp:1047`):
/// short-circuits once initialised; otherwise the DLL is probed next to
/// the executable first, then in the `awesomium/` resource path.
pub fn check_web_core(ctx: &mut WebCoreContext) -> bool {
    if ctx.initialised {
        return true;
    }

    let mut working_dir_path = APP_DIRECTORY.to_string();

    if !ctx.dll_beside_exe {
        let rel_dll_path = format!("{}{}", AWESOMIUM_RES_PATH, AWESOMIUM_DLL);
        if !ctx.dll_in_res_path {
            ctx.logs.push(format!(
                "Awesomium failed to initialise: '{}' not found.\n",
                AWESOMIUM_DLL
            ));
            return false;
        }

        // get full path without trailing slash; '/' becomes '\\' before
        // the base path is cut at the last backslash.
        let resolved = ctx
            .res_dll_resolved
            .clone()
            .unwrap_or_else(|| rel_dll_path.clone());
        match resolved.rfind('\\') {
            None => {
                ctx.logs
                    .push("Awesomium failed to initialise.\n".to_string());
                return false;
            }
            Some(idx) => {
                working_dir_path = resolved[..idx].to_string();
            }
        }
    }

    check_web_core_impl(ctx, &working_dir_path)
}

/// `checkWebCoreImpl` (`awesomium_provider.cpp:1090`): the forced-path
/// `LoadLibraryA`, the `WebConfig` and `WebCore::Initialize`.
pub fn check_web_core_impl(ctx: &mut WebCoreContext, base_path: &str) -> bool {
    let dll_path = format!("{}\\{}", base_path, AWESOMIUM_DLL);
    let process_path = format!("{}\\{}", base_path, AWESOMIUM_PROCESS);
    let log_path = format!("{}\\{}", base_path, AWESOMIUM_LOG);

    ctx.core_ops.push(WebCoreOp::LoadLibrary {
        path: dll_path.clone(),
    });
    if ctx.load_library_fails {
        ctx.logs.push(format!("Could not load '{}'\n", dll_path));
        return false;
    }
    ctx.module_loaded = true;
    ctx.module_path = Some(dll_path);

    ctx.core_ops.push(WebCoreOp::Initialize {
        child_process_path: process_path,
        log_path,
        plugin_path: base_path.to_string(),
        package_path: base_path.to_string(),
    });
    if ctx.initialize_fails {
        ctx.logs
            .push("Awesomium failed to initialise.\n".to_string());
        ctx.initialised = false;
    } else {
        // MF_ASSERT(pResourceInterceptor_ == NULL) — a fresh context never
        // has one; fini deletes it before this can run again.
        assert!(
            !ctx.interceptor_installed,
            "MF_ASSERT: pResourceInterceptor_ == NULL"
        );
        ctx.interceptor_installed = true;
        ctx.core_ops
            .push(WebCoreOp::SetResourceInterceptor { installed: true });
        ctx.logs
            .push(format!("Awesomium initialised: version {}.\n", ctx.version));
        ctx.initialised = true;
    }

    ctx.initialised
}

/// `PyAwesomiumProvider::init` (`awesomium_provider.cpp:1037`): empty —
/// the WebCore is initialised lazily on the first provider, so the DLL is
/// never loaded on distributions that don't need it.
pub fn web_core_init(_ctx: &mut WebCoreContext) {}

/// `PyAwesomiumProvider::fini` (`awesomium_provider.cpp:1136`): force
/// cleanup on outstanding providers, drop the interceptor, shut the core
/// down and free the library. The interceptor delete is *unconditional*.
pub fn web_core_fini(ctx: &mut WebCoreContext) {
    if ctx.initialised {
        for provider in ctx.live_providers() {
            provider.borrow_mut().cleanup();
        }
        ctx.interceptor_installed = false;
        ctx.core_ops
            .push(WebCoreOp::SetResourceInterceptor { installed: false });
        ctx.core_ops.push(WebCoreOp::Shutdown);
    }
    ctx.interceptor_installed = false;
    ctx.initialised = false;

    if ctx.module_loaded {
        ctx.core_ops.push(WebCoreOp::FreeLibrary);
        ctx.module_loaded = false;
        ctx.module_path = None;
    }
}

/// `PyAwesomiumProvider::tick` (`awesomium_provider.cpp:1165`): the core
/// pumps its message loop, then every provider refreshes its texture.
pub fn web_core_tick(ctx: &mut WebCoreContext) {
    if !ctx.initialised {
        return;
    }

    if ctx.instance() {
        ctx.core_ops.push(WebCoreOp::Update);
    }

    for provider in ctx.live_providers() {
        provider.borrow_mut().update_texture();
    }
}

// ---------------------------------------------------------------------------
// PyAwesomiumProvider
// ---------------------------------------------------------------------------

/// The `BigWorld.AwesomiumProvider` factory argument values.
#[derive(Clone, PartialEq, Debug)]
pub enum PyFactoryArg {
    Int(i64),
    Float(f64),
    Str(String),
    Other,
}

/// The script-level error the factory raises.
#[derive(Clone, PartialEq, Debug)]
pub struct FactoryError {
    pub message: String,
}

/// The `PyAwesomiumProvider` port: an Awesomium web view rendered into a
/// dynamic texture, exposed through the `PyTextureProvider` interface.
pub struct PyAwesomiumProvider {
    view: Option<Rc<RefCell<WebViewState>>>,
    texture: Option<AwesomiumTexture>,
    listener: Option<WebListener>,
}

impl PyAwesomiumProvider {
    /// The constructor workflow (`awesomium_provider.cpp:554`): lazy
    /// WebCore check, `CreateWebView`, listener wiring, dynamic texture,
    /// registration. Returns the provider (usable or not) registered in
    /// the context's provider set — the C++ inserts itself *only* when the
    /// view was created, and the port matches that.
    pub fn new(
        ctx: &mut WebCoreContext,
        width: i32,
        height: i32,
    ) -> Rc<RefCell<PyAwesomiumProvider>> {
        let provider = Rc::new(RefCell::new(PyAwesomiumProvider {
            view: None,
            texture: None,
            listener: None,
        }));

        if !check_web_core(ctx) {
            return provider;
        }

        if !ctx.instance() {
            ctx.logs.push(
                "PyAwesomiumProvider::PyAwesomiumProvider: Awesomium not initialisd.\n".to_string(),
            );
            return provider;
        }

        if ctx.create_view_fails {
            ctx.logs.push(format!(
                "PyAwesomiumProvider::PyAwesomiumProvider: Failed to create {}x{} web view.\n",
                width, height
            ));
            return provider;
        }

        let id = ctx.views.len();
        let view = Rc::new(RefCell::new(WebViewState::new(id, width, height)));
        view.borrow_mut().view_listener_installed = true;
        view.borrow_mut().load_listener_installed = true;
        view.borrow_mut().ops.push(WebOp::SetViewListener);
        view.borrow_mut().ops.push(WebOp::SetLoadListener);
        ctx.views.push(Rc::clone(&view));

        let texture = AwesomiumTexture::new(width, height);
        let listener = WebListener::new();
        {
            let mut provider = provider.borrow_mut();
            provider.view = Some(view);
            provider.texture = Some(texture);
            provider.listener = Some(listener);
        }

        ctx.providers.push(Rc::downgrade(&provider));
        provider
    }

    /// The static python factory (`awesomium_provider.cpp:1015`):
    /// `PyArg_ParseTuple("ii")` with the verbatim TypeError.
    pub fn factory(args: &[PyFactoryArg]) -> Result<(i32, i32), FactoryError> {
        let parsed = match args {
            [PyFactoryArg::Int(w), PyFactoryArg::Int(h)] => Some((*w as i32, *h as i32)),
            _ => None,
        };
        match parsed {
            Some((w, h)) => Ok((w, h)),
            None => Err(FactoryError {
                message:
                    "BigWorld.PyAwesomiumProvider() expects width and height integer arguments"
                        .to_string(),
            }),
        }
    }

    /// Whether the view survived construction.
    pub fn has_view(&self) -> bool {
        self.view.is_some()
    }

    /// `updateTexture` (`awesomium_provider.cpp:611`): blits the surface;
    /// silent no-op without a view or texture (e.g. after cleanup).
    pub fn update_texture(&mut self) -> bool {
        let (Some(view), Some(texture)) = (&self.view, &mut self.texture) else {
            return false;
        };
        texture.copy_web_view(&view.borrow())
    }

    /// `PyTextureProvider::texture` override (`awesomium_provider.cpp:629`):
    /// every texture fetch first refreshes from the web view.
    pub fn texture(&mut self) -> Option<&AwesomiumTexture> {
        self.update_texture();
        self.texture.as_ref()
    }

    /// `loadURL` — the full protocol prefix is required by the SDK
    /// (`http://…` not `www.…`).
    pub fn load_url(&mut self, url: &str) {
        let Some(view) = &self.view else {
            return;
        };
        let mut view = view.borrow_mut();
        view.ops.push(WebOp::LoadURL {
            url: url.to_string(),
        });
        view.url = url.to_string();
        view.loading = true;
    }

    /// `isLoading` accessor: false without a view.
    pub fn is_loading(&self) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        view.borrow().loading
    }

    /// `url` accessor: "" without a view.
    pub fn url(&self) -> String {
        let Some(view) = &self.view else {
            return String::new();
        };
        view.borrow().url.clone()
    }

    /// `width` accessor — reads the *texture*, not the view; 0 without.
    pub fn width(&self) -> u32 {
        let Some(texture) = &self.texture else {
            return 0;
        };
        texture.width() as u32
    }

    /// `height` accessor — reads the *texture*, not the view; 0 without.
    pub fn height(&self) -> u32 {
        let Some(texture) = &self.texture else {
            return 0;
        };
        texture.height() as u32
    }

    fn with_view<R>(&self, f: impl FnOnce(&mut WebViewState) -> R) -> Option<R> {
        let view = self.view.as_ref()?;
        let mut view = view.borrow_mut();
        Some(f(&mut view))
    }

    /// `stop`.
    pub fn stop(&mut self) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::Stop);
            view.loading = false;
        });
    }

    /// `reload(ignoreCache)`.
    pub fn reload(&mut self, ignore_cache: bool) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::Reload { ignore_cache });
        });
    }

    /// `goBack`.
    pub fn go_back(&mut self) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::GoBack);
        });
    }

    /// `goForward`.
    pub fn go_forward(&mut self) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::GoForward);
        });
    }

    /// `focus` — required before injecting key events (blinking cursor).
    pub fn focus(&mut self) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::Focus);
            view.focused = true;
        });
    }

    /// `unfocus`.
    pub fn unfocus(&mut self) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::Unfocus);
            view.focused = false;
        });
    }

    /// `resize(width, height)` — the *texture* is resized unconditionally
    /// (`pTexture_.get()` is dereferenced even when NULL-guarding only
    /// guards the view); then the view is resized.
    pub fn resize(&mut self, width: i32, height: i32) {
        if let Some(texture) = &mut self.texture {
            texture.resize(width, height);
        }
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::Resize { width, height });
            view.width = width;
            view.height = height;
            view.surface = SurfaceState::new(width, height);
        });
    }

    /// `injectKeyEvent` (`awesomium_provider.cpp:830`): mouse buttons
    /// become `InjectMouseDown/Up`; everything else becomes a
    /// `WebKeyboardEvent` (plus a separate `WM_CHAR` event when the key
    /// carries a character).
    pub fn inject_key_event(&mut self, event: &KeyEvent) {
        let Some(view) = self.with_view(|_| true) else {
            return;
        };
        let _ = view;

        if event.mouse_button {
            let key = event.key.unwrap_or(KeyCode::Unknown);
            let button = convert_mouse_button(key);
            self.with_view(|view| {
                if event.key_down {
                    view.ops.push(WebOp::InjectMouseDown { button });
                } else {
                    view.ops.push(WebOp::InjectMouseUp { button });
                }
            });
            return;
        }

        let kind = if event.key_down {
            WebKeyEventType::KeyDown
        } else {
            WebKeyEventType::KeyUp
        };
        let key = event.key.unwrap_or(KeyCode::Unknown);
        let virtual_key_code = vk_map_to_vkey(key);
        let modifiers = convert_modifiers(event);
        let key_identifier = key_identifier_from_vk(virtual_key_code);
        let native_key_code = map_virtual_key_vk_to_vsc(virtual_key_code);
        self.with_view(|view| {
            view.ops.push(WebOp::InjectKeyboardEvent {
                kind: kind.clone(),
                is_system_key: false,
                virtual_key_code,
                modifiers,
                key_identifier: key_identifier.clone(),
                native_key_code,
                char_text: String::new(),
            });

            // Insert a separate event for characters.
            if event.utf16.first().is_some_and(|first| *first != 0) {
                let char_text = String::from_utf16_lossy(&event.utf16);
                view.ops.push(WebOp::InjectKeyboardEvent {
                    kind: WebKeyEventType::Char,
                    is_system_key: false,
                    virtual_key_code: 0,
                    modifiers: 0,
                    key_identifier: String::new(),
                    native_key_code: 0,
                    char_text,
                });
            }
        });
    }

    /// `injectMouseMoveEvent(x, y)`.
    pub fn inject_mouse_move_event(&mut self, x: i32, y: i32) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::InjectMouseMove { x, y });
        });
    }

    /// `injectMouseWheelEvent(dz)` — the horizontal distance is always 0.
    pub fn inject_mouse_wheel_event(&mut self, dz: i32) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::InjectMouseWheel { dz, dw: 0 });
        });
    }

    /// `cleanup` (`awesomium_provider.cpp:928`): the provider becomes
    /// unusable; the view destroys itself last.
    pub fn cleanup(&mut self) {
        self.texture = None;
        if let Some(view) = self.view.take() {
            let mut view = view.borrow_mut();
            view.load_listener_installed = false;
            view.view_listener_installed = false;
            view.ops.push(WebOp::Destroy);
            view.destroyed = true;
        }
        self.listener = None;
    }

    /// The `script` attribute setter — silently ignored once the listener
    /// is gone.
    pub fn set_script_object(&mut self, object: Option<&str>) {
        let Some(listener) = &mut self.listener else {
            return;
        };
        listener.set_object(object);
    }

    /// The `script` attribute getter — `None` without a listener.
    pub fn script_object(&self) -> Option<String> {
        self.listener
            .as_ref()
            .and_then(|listener| listener.object().map(|name| name.to_string()))
    }

    /// The listener's dispatched calls (the script callback 对照).
    pub fn script_calls(&self) -> Vec<ScriptCall> {
        self.listener
            .as_ref()
            .map(|listener| listener.calls().to_vec())
            .unwrap_or_default()
    }

    /// `executeJavascript(script, frameXPath)` — asynchronous.
    pub fn execute_javascript(&mut self, script: &str, frame_xpath: &str) {
        let _ = self.with_view(|view| {
            view.ops.push(WebOp::ExecuteJavascript {
                script: script.to_string(),
                frame_xpath: frame_xpath.to_string(),
            });
        });
    }

    /// `executeJavascriptWithResult(script, frameXPath)` — synchronous;
    /// "" without a view or a queued result.
    pub fn execute_javascript_with_result(&mut self, script: &str, frame_xpath: &str) -> String {
        let Some(view) = &self.view else {
            return String::new();
        };
        let mut view = view.borrow_mut();
        view.ops.push(WebOp::ExecuteJavascriptWithResult {
            script: script.to_string(),
            frame_xpath: frame_xpath.to_string(),
        });
        view.js_results.pop_front().unwrap_or_default()
    }

    /// Queue a result for the next `executeJavascriptWithResult` (the
    /// absent SDK's page-side behaviour).
    pub fn queue_js_result(&mut self, result: &str) {
        if let Some(view) = &self.view {
            view.borrow_mut().js_results.push_back(result.to_string());
        }
    }

    /// Drive the listener from the SDK side (the WebView event pump face).
    pub fn listener_mut(&mut self) -> Option<&mut WebListener> {
        self.listener.as_mut()
    }
}

// ---------------------------------------------------------------------------
// WebApp — the main-loop task
// ---------------------------------------------------------------------------

/// The main-loop task name `WebApp` registers.
pub const WEB_MAIN_LOOP_TASK: &str = "Web/App";

/// The `WebApp` task face (`client/web_app.cpp`): owns nothing itself —
/// init/fini/tick forward to the provider statics under the
/// `ENABLE_AWESOMIUM` gate.
pub struct WebApp {
    registered: bool,
}

impl WebApp {
    pub fn new() -> WebApp {
        WebApp { registered: true }
    }

    /// Whether the "Web/App" task is in the main loop.
    pub fn registered(&self) -> bool {
        self.registered
    }

    /// `WebApp::init`: refuses while quiting; the provider init is the
    /// empty symmetry stub.
    pub fn init(&mut self, ctx: &mut WebCoreContext, is_quiting: bool) -> bool {
        if is_quiting {
            return false;
        }
        web_core_init(ctx);
        true
    }

    /// `WebApp::fini`.
    pub fn fini(&mut self, ctx: &mut WebCoreContext) {
        web_core_fini(ctx);
    }

    /// `WebApp::tick`.
    pub fn tick(&mut self, ctx: &mut WebCoreContext) {
        web_core_tick(ctx);
    }
}

impl Default for WebApp {
    fn default() -> WebApp {
        WebApp::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_exe_dll() -> WebCoreContext {
        let mut ctx = WebCoreContext::new();
        ctx.dll_beside_exe = true;
        ctx
    }

    fn provider(ctx: &mut WebCoreContext, w: i32, h: i32) -> Rc<RefCell<PyAwesomiumProvider>> {
        PyAwesomiumProvider::new(ctx, w, h)
    }

    fn view_ops(ctx: &WebCoreContext, id: usize) -> Vec<WebOp> {
        ctx.views[id].borrow().ops().to_vec()
    }

    fn key_event(key: KeyCode, down: bool, alt: bool, ctrl: bool, shift: bool) -> KeyEvent {
        KeyEvent {
            key: Some(key),
            key_down: down,
            mouse_button: false,
            alt,
            ctrl,
            shift,
            utf16: Vec::new(),
        }
    }

    #[test]
    fn mime_table_and_sensible_default() {
        assert_eq!(extension_to_mime("html"), "text/html");
        assert_eq!(extension_to_mime("htm"), "text/html");
        assert_eq!(extension_to_mime("htmls"), "text/html");
        assert_eq!(extension_to_mime("css"), "text/css");
        assert_eq!(extension_to_mime("xml"), "text/xml");
        assert_eq!(extension_to_mime("xhtml"), "text/xml");
        assert_eq!(extension_to_mime("js"), "text/javascript");
        assert_eq!(extension_to_mime("jpeg"), "image/jpeg");
        assert_eq!(extension_to_mime("jpg"), "image/jpeg");
        assert_eq!(extension_to_mime("gif"), "image/gif");
        assert_eq!(extension_to_mime("png"), "image/png");
        assert_eq!(extension_to_mime("swf"), "application/x-shockwave-flash");
        // Sensible default?
        assert_eq!(extension_to_mime("mystery"), "text/plain");
    }

    #[test]
    fn resource_extension_after_last_dot() {
        assert_eq!(resource_extension("page/index.html"), "html");
        assert_eq!(resource_extension("archive.tar.gz"), "gz");
        assert_eq!(resource_extension("noextension"), "");
    }

    #[test]
    fn interceptor_serves_file_scheme_from_resource_system() {
        let mut system = ResourceSystem::new();
        system.add_file("game/html/index.html", b"<html></html>");

        let response = resource_interceptor_on_request(
            &system,
            &ResourceRequest {
                scheme: "file".to_string(),
                path: "game/html/index.html".to_string(),
            },
        )
        .expect("file scheme hit");
        assert_eq!(response.data, b"<html></html>".to_vec());
        assert_eq!(response.mime, "text/html");

        // A read failure yields NULL (SDK falls back to its own loading).
        assert!(
            resource_interceptor_on_request(
                &system,
                &ResourceRequest {
                    scheme: "file".to_string(),
                    path: "game/html/missing.html".to_string(),
                }
            )
            .is_none()
        );

        // A foreign scheme is left entirely to the SDK.
        assert!(
            resource_interceptor_on_request(
                &system,
                &ResourceRequest {
                    scheme: "http".to_string(),
                    path: "www.google.com/".to_string(),
                }
            )
            .is_none()
        );
    }

    #[test]
    fn convert_mouse_button_maps_three_buttons_and_asserts_otherwise() {
        assert_eq!(
            convert_mouse_button(KeyCode::LeftMouse),
            MouseButtonKind::Left
        );
        assert_eq!(
            convert_mouse_button(KeyCode::RightMouse),
            MouseButtonKind::Right
        );
        assert_eq!(
            convert_mouse_button(KeyCode::MiddleMouse),
            MouseButtonKind::Middle
        );
        let result = std::panic::catch_unwind(|| convert_mouse_button(KeyCode::Return));
        assert!(result.is_err());
    }

    #[test]
    fn convert_modifiers_masks() {
        assert_eq!(
            convert_modifiers(&key_event(KeyCode::KeyA, true, false, false, false)),
            0
        );
        assert_eq!(
            convert_modifiers(&key_event(KeyCode::KeyA, true, true, false, false)),
            K_MOD_ALT_KEY
        );
        assert_eq!(
            convert_modifiers(&key_event(KeyCode::KeyA, true, false, true, false)),
            K_MOD_CONTROL_KEY
        );
        assert_eq!(
            convert_modifiers(&key_event(KeyCode::KeyA, true, false, false, true)),
            K_MOD_SHIFT_KEY
        );
        assert_eq!(
            convert_modifiers(&key_event(KeyCode::KeyA, true, true, true, true)),
            K_MOD_ALT_KEY | K_MOD_CONTROL_KEY | K_MOD_SHIFT_KEY
        );
    }

    #[test]
    fn vk_map_reverse_scan_and_unknown_gives_zero() {
        assert_eq!(vk_map_to_vkey(KeyCode::Return), 0x0D);
        assert_eq!(vk_map_to_vkey(KeyCode::Escape), 0x1B);
        assert_eq!(vk_map_to_vkey(KeyCode::Space), 0x20);
        assert_eq!(vk_map_to_vkey(KeyCode::KeyA), 0x41);
        assert_eq!(vk_map_to_vkey(KeyCode::KeyB), 0x42);
        assert_eq!(vk_map_to_vkey(KeyCode::LeftMouse), 0x01);
        assert_eq!(vk_map_to_vkey(KeyCode::RightMouse), 0x02);
        assert_eq!(vk_map_to_vkey(KeyCode::MiddleMouse), 0x04);
        assert_eq!(vk_map_to_vkey(KeyCode::Unknown), 0);
        assert_eq!(map_virtual_key_vk_to_vsc(0x141), 0x41);
        assert!(key_identifier_from_vk(0x0D).starts_with("VKey_"));
    }

    #[test]
    fn texture_skips_copy_for_a_destroyed_view() {
        let mut texture = AwesomiumTexture::new(8, 8);
        let mut view = WebViewState::new(0, 8, 8);
        view.destroyed = true;
        // A destroyed view presents no surface: the copy gate closes.
        assert!(!texture.copy_web_view(&view));
    }

    #[test]
    fn factory_parses_two_ints_and_raises_otherwise() {
        assert_eq!(
            PyAwesomiumProvider::factory(&[PyFactoryArg::Int(64), PyFactoryArg::Int(48)]),
            Ok((64, 48))
        );

        let err = PyAwesomiumProvider::factory(&[PyFactoryArg::Float(64.0), PyFactoryArg::Int(48)])
            .expect_err("float is not an int");
        assert_eq!(
            err.message,
            "BigWorld.PyAwesomiumProvider() expects width and height integer arguments"
        );

        let err = PyAwesomiumProvider::factory(&[PyFactoryArg::Int(64)])
            .expect_err("one argument is not two");
        assert_eq!(
            err.message,
            "BigWorld.PyAwesomiumProvider() expects width and height integer arguments"
        );

        let err = PyAwesomiumProvider::factory(&[]).expect_err("no arguments is not two");
        assert_eq!(
            err.message,
            "BigWorld.PyAwesomiumProvider() expects width and height integer arguments"
        );
    }

    #[test]
    fn constructor_workflow_creates_view_listener_and_white_texture() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);
        let p = p.borrow();

        assert!(p.has_view());
        assert_eq!(p.width(), 64);
        assert_eq!(p.height(), 48);
        assert!(ctx.initialised);
        assert!(ctx.module_loaded);
        assert!(ctx.interceptor_installed);
        assert_eq!(ctx.views.len(), 1);

        // The dynamic texture starts white: every byte 0xFF.
        let texture = p.texture.as_ref().unwrap();
        let pixels = texture.pixels.as_ref().unwrap();
        assert_eq!(pixels.len(), 64 * 48 * 4);
        assert!(pixels.iter().all(|byte| *byte == 0xFF));
        assert_eq!(texture.texture_memory_used(), 64 * 48 * 4);
        assert!(texture.recreate_for_d3d_ex_device());
        assert_eq!(texture.bytes_per_pixel(), 4);

        // The provider registered itself only because the view was made.
        assert_eq!(ctx.providers.len(), 1);
        assert!(ctx.live_providers().len() == 1);

        // Listener wiring happened before anything else touched the view.
        assert_eq!(
            view_ops(&ctx, 0),
            &[WebOp::SetViewListener, WebOp::SetLoadListener]
        );
    }

    #[test]
    fn constructor_without_dll_records_error_and_stays_unusable() {
        let mut ctx = WebCoreContext::new();
        let p = provider(&mut ctx, 64, 48);

        assert!(!p.borrow().has_view());
        assert!(!ctx.initialised);
        assert!(!ctx.module_loaded);
        assert_eq!(ctx.views.len(), 0);
        assert_eq!(ctx.providers.len(), 0);
        assert_eq!(
            ctx.logs(),
            &["Awesomium failed to initialise: 'awesomium.dll' not found.\n".to_string()]
        );

        // All accessors report their empty values.
        assert_eq!(p.borrow().width(), 0);
        assert_eq!(p.borrow().height(), 0);
        assert!(!p.borrow().is_loading());
        assert_eq!(p.borrow().url(), "");
        {
            let mut p = p.borrow_mut();
            assert!(!p.update_texture());
            assert!(p.texture().is_none());
        }
    }

    #[test]
    fn constructor_falls_back_to_res_path_dll() {
        let mut ctx = WebCoreContext::new();
        ctx.dll_in_res_path = true;
        ctx.res_dll_resolved = Some("game\\awesomium\\awesomium.dll".to_string());
        let p = provider(&mut ctx, 32, 32);
        assert!(p.borrow().has_view());

        // The base path cut at the last backslash feeds the config.
        assert!(matches!(
            &ctx.core_ops[0],
            WebCoreOp::LoadLibrary { path } if path == "game\\awesomium\\awesomium.dll"
        ));
        assert!(matches!(
            &ctx.core_ops[1],
            WebCoreOp::Initialize {
                child_process_path,
                log_path,
                plugin_path,
                package_path,
            } if child_process_path == "game\\awesomium\\awesomium_process.exe"
                && log_path == "game\\awesomium\\awesomium.log"
                && plugin_path == "game\\awesomium"
                && package_path == "game\\awesomium"
        ));
        assert_eq!(
            ctx.logs(),
            &["Awesomium initialised: version 1.7.0.0.\n".to_string()]
        );
    }

    #[test]
    fn constructor_res_path_without_backslash_records_error() {
        let mut ctx = WebCoreContext::new();
        ctx.dll_in_res_path = true;
        ctx.res_dll_resolved = Some("awesomium.dll".to_string());
        let p = provider(&mut ctx, 32, 32);
        assert!(!p.borrow().has_view());
        assert_eq!(
            ctx.logs(),
            &["Awesomium failed to initialise.\n".to_string()]
        );
        assert!(!ctx.module_loaded);
    }

    #[test]
    fn constructor_res_path_unresolved_falls_back_to_the_relative_dll_path() {
        let mut ctx = WebCoreContext::new();
        ctx.dll_in_res_path = true;
        // No resolved filename: the relative "awesomium/awesomium.dll"
        // stands in, and it carries no backslash — the same npos branch.
        ctx.res_dll_resolved = None;
        let p = provider(&mut ctx, 32, 32);
        assert!(!p.borrow().has_view());
        assert_eq!(
            ctx.logs(),
            &["Awesomium failed to initialise.\n".to_string()]
        );
        assert!(!ctx.module_loaded);
        assert_eq!(WebCoreContext::default().views.len(), 0);
    }

    #[test]
    fn load_library_failure_is_critical_and_uninitialised() {
        let mut ctx = ctx_with_exe_dll();
        ctx.load_library_fails = true;
        let p = provider(&mut ctx, 32, 32);
        assert!(!p.borrow().has_view());
        assert!(!ctx.initialised);
        assert!(!ctx.module_loaded);
        assert!(matches!(
            ctx.logs(),
            [log] if log.contains("Could not load 'game\\awesomium.dll'")
        ));
    }

    #[test]
    fn initialize_failure_keeps_module_loaded_but_core_off() {
        let mut ctx = ctx_with_exe_dll();
        ctx.initialize_fails = true;
        let p = provider(&mut ctx, 32, 32);
        assert!(!p.borrow().has_view());
        assert!(!ctx.initialised);
        // The DLL *was* loaded before Initialize ran; fini frees it.
        assert!(ctx.module_loaded);
        assert_eq!(
            ctx.logs(),
            &["Awesomium failed to initialise.\n".to_string()]
        );

        web_core_fini(&mut ctx);
        assert!(!ctx.module_loaded);
        assert!(matches!(ctx.core_ops.last(), Some(WebCoreOp::FreeLibrary)));
    }

    #[test]
    fn create_view_failure_records_dimensions_and_no_registration() {
        let mut ctx = ctx_with_exe_dll();
        ctx.create_view_fails = true;
        let p = provider(&mut ctx, 80, 60);
        let p = p.borrow();
        assert!(!p.has_view());
        assert!(ctx.initialised);
        assert_eq!(ctx.views.len(), 0);
        // Not registered, so tick would not reach it.
        assert!(ctx.live_providers().is_empty());
        assert_eq!(
            ctx.logs(),
            &[
                "Awesomium initialised: version 1.7.0.0.\n".to_string(),
                "PyAwesomiumProvider::PyAwesomiumProvider: Failed to create 80x60 web view.\n"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn second_constructor_short_circuits_the_core_check() {
        let mut ctx = ctx_with_exe_dll();
        let first = provider(&mut ctx, 64, 48);
        let first_ops = ctx.core_ops.len();
        let _second = provider(&mut ctx, 32, 32);
        // checkWebCore returns early when already initialised: no new
        // LoadLibrary/Initialize ops.
        assert_eq!(ctx.core_ops.len(), first_ops);
        assert_eq!(ctx.views.len(), 2);
        let _ = first;
    }

    #[test]
    fn navigation_and_focus_forward_to_view_ops() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);

        p.borrow_mut().load_url("http://www.google.com/");
        assert_eq!(p.borrow().url(), "http://www.google.com/");
        assert!(p.borrow().is_loading());
        p.borrow_mut().stop();
        assert!(!p.borrow().is_loading());
        p.borrow_mut().reload(true);
        p.borrow_mut().reload(false);
        p.borrow_mut().go_back();
        p.borrow_mut().go_forward();
        p.borrow_mut().focus();
        assert!(p.borrow().view.as_ref().unwrap().borrow().focused);
        p.borrow_mut().unfocus();
        assert!(!p.borrow().view.as_ref().unwrap().borrow().focused);

        let expected = vec![
            WebOp::SetViewListener,
            WebOp::SetLoadListener,
            WebOp::LoadURL {
                url: "http://www.google.com/".to_string(),
            },
            WebOp::Stop,
            WebOp::Reload { ignore_cache: true },
            WebOp::Reload {
                ignore_cache: false,
            },
            WebOp::GoBack,
            WebOp::GoForward,
            WebOp::Focus,
            WebOp::Unfocus,
        ];
        assert_eq!(view_ops(&ctx, 0), expected);
    }

    #[test]
    fn resize_recreates_texture_and_resizes_view() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);

        p.borrow_mut().resize(128, 96);
        {
            let p = p.borrow();
            assert_eq!(p.width(), 128);
            assert_eq!(p.height(), 96);
            // The recreated texture is white again and forced.
            let texture = p.texture.as_ref().unwrap();
            let pixels = texture.pixels.as_ref().unwrap();
            assert_eq!(pixels.len(), 128 * 96 * 4);
            assert!(pixels.iter().all(|byte| *byte == 0xFF));
        }

        let ops = view_ops(&ctx, 0);
        assert!(matches!(
            &ops[2],
            WebOp::Resize {
                width: 128,
                height: 96
            }
        ));
        let view = ctx.views[0].borrow();
        assert_eq!(view.surface.width, 128);
        assert_eq!(view.surface.height, 96);
    }

    #[test]
    fn update_texture_copies_only_when_dirty_or_forced() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 8, 8);

        // First copy is forced (fresh texture), even with a clean surface.
        ctx.views[0].borrow_mut().surface.buffer[..4].copy_from_slice(&[1, 2, 3, 4]);
        assert!(p.borrow_mut().update_texture());
        assert_eq!(
            p.borrow()
                .texture
                .as_ref()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()[..4],
            [1, 2, 3, 4]
        );

        // Second copy is skipped: surface clean, force consumed.
        ctx.views[0].borrow_mut().surface.buffer[..4].copy_from_slice(&[9, 9, 9, 9]);
        assert!(!p.borrow_mut().update_texture());
        assert_eq!(
            p.borrow()
                .texture
                .as_ref()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()[..4],
            [1, 2, 3, 4]
        );

        // Marking the surface dirty re-enables the copy.
        ctx.views[0].borrow_mut().surface.dirty = true;
        assert!(p.borrow_mut().update_texture());
        assert_eq!(
            p.borrow()
                .texture
                .as_ref()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()[..4],
            [9, 9, 9, 9]
        );
    }

    #[test]
    fn texture_fetch_refreshes_from_the_web_view() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 8, 8);
        ctx.views[0].borrow_mut().surface.buffer[..1].copy_from_slice(&[7]);
        // texture() refreshes before handing the texture out.
        let first_pixel = p.borrow_mut().texture().unwrap().pixels.as_ref().unwrap()[0];
        assert_eq!(first_pixel, 7);
    }

    #[test]
    fn surface_dimension_mismatch_asserts() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 8, 8);
        ctx.views[0].borrow_mut().surface = SurfaceState::new(16, 16);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            p.borrow_mut().update_texture();
        }));
        assert!(result.is_err());
    }

    #[test]
    fn lock_failure_logs_error_and_clears_force() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 8, 8);
        ctx.views[0].borrow_mut().surface.buffer[..4].copy_from_slice(&[1, 2, 3, 4]);
        p.borrow_mut().texture.as_mut().unwrap().lock_fails = true;

        // The lock is attempted (forced), fails, logs, and clears force.
        assert!(p.borrow_mut().update_texture());
        let p = p.borrow();
        let texture = p.texture.as_ref().unwrap();
        assert!(texture.logs.iter().any(|log| {
            log.starts_with("AwesomiumTexture::copyWebView: failed to lock D3D texture")
        }));
        assert_eq!(texture.pixels.as_ref().unwrap()[..4], [0xFF; 4]);
    }

    #[test]
    fn cleanup_destroys_view_and_provider_becomes_unusable() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);
        p.borrow_mut().set_script_object(Some("gui"));

        p.borrow_mut().cleanup();
        {
            let mut p = p.borrow_mut();
            assert!(!p.has_view());
            assert!(p.texture.is_none());
            assert!(p.script_object().is_none());
            assert_eq!(p.width(), 0);
            assert_eq!(p.height(), 0);
            assert_eq!(p.url(), "");
            assert!(!p.is_loading());
            assert!(!p.update_texture());
            assert!(p.texture().is_none());
        }
        // Setter is silently ignored without a listener.
        p.borrow_mut().set_script_object(Some("late"));
        assert!(p.borrow().script_object().is_none());
        // A second cleanup is a no-op, and a queued result has nowhere
        // to go.
        p.borrow_mut().cleanup();
        p.borrow_mut().queue_js_result("orphan");
        assert!(p.borrow().texture.is_none());

        let ops = view_ops(&ctx, 0);
        assert!(matches!(ops.last(), Some(WebOp::Destroy)));
        let view = ctx.views[0].borrow();
        assert!(view.destroyed);
        assert!(!view.view_listener_installed);
        assert!(!view.load_listener_installed);
    }

    #[test]
    fn inject_mouse_buttons_route_by_key_code() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);

        p.borrow_mut()
            .inject_key_event(&KeyEvent::mouse(KeyCode::LeftMouse, true));
        p.borrow_mut()
            .inject_key_event(&KeyEvent::mouse(KeyCode::LeftMouse, false));
        p.borrow_mut()
            .inject_key_event(&KeyEvent::mouse(KeyCode::RightMouse, true));
        p.borrow_mut()
            .inject_key_event(&KeyEvent::mouse(KeyCode::MiddleMouse, false));

        let ops = view_ops(&ctx, 0);
        assert_eq!(
            &ops[2..],
            &[
                WebOp::InjectMouseDown {
                    button: MouseButtonKind::Left
                },
                WebOp::InjectMouseUp {
                    button: MouseButtonKind::Left
                },
                WebOp::InjectMouseDown {
                    button: MouseButtonKind::Right
                },
                WebOp::InjectMouseUp {
                    button: MouseButtonKind::Middle
                },
            ]
        );
    }

    #[test]
    fn inject_keyboard_event_carries_vk_modifiers_and_char_payload() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);

        let mut event = key_event(KeyCode::KeyA, true, false, true, true);
        event.utf16 = vec![0x0061]; // 'a'
        p.borrow_mut().inject_key_event(&event);
        p.borrow_mut()
            .inject_key_event(&KeyEvent::key_up(KeyCode::KeyA));
        // The helper constructor and the zero first code unit (no char
        // event) close out the remaining inject paths.
        p.borrow_mut()
            .inject_key_event(&KeyEvent::key_down(KeyCode::Return));
        let mut nul = key_event(KeyCode::Space, true, false, false, false);
        nul.utf16 = vec![0x0000];
        p.borrow_mut().inject_key_event(&nul);

        let ops = view_ops(&ctx, 0);
        assert_eq!(ops.len(), 7);
        match &ops[2] {
            WebOp::InjectKeyboardEvent {
                kind,
                is_system_key,
                virtual_key_code,
                modifiers,
                key_identifier,
                native_key_code,
                char_text,
            } => {
                assert!(matches!(kind, WebKeyEventType::KeyDown));
                assert!(!is_system_key);
                assert_eq!(*virtual_key_code, 0x41);
                assert_eq!(*modifiers, K_MOD_CONTROL_KEY | K_MOD_SHIFT_KEY);
                assert_eq!(key_identifier, &key_identifier_from_vk(0x41));
                assert_eq!(*native_key_code, map_virtual_key_vk_to_vsc(0x41));
                assert!(char_text.is_empty());
            }
            other => panic!("expected keyboard event, got {:?}", other),
        }
        // The character payload goes through as a separate WM_CHAR event.
        match &ops[3] {
            WebOp::InjectKeyboardEvent {
                kind, char_text, ..
            } => {
                assert!(matches!(kind, WebKeyEventType::Char));
                assert_eq!(char_text, "a");
            }
            other => panic!("expected char event, got {:?}", other),
        }
        match &ops[4] {
            WebOp::InjectKeyboardEvent { kind, .. } => {
                assert!(matches!(kind, WebKeyEventType::KeyUp));
            }
            other => panic!("expected key-up, got {:?}", other),
        }
        // The Return key-down carries no character: no WM_CHAR follows.
        assert!(matches!(
            &ops[5],
            WebOp::InjectKeyboardEvent {
                kind: WebKeyEventType::KeyDown,
                virtual_key_code: 0x0D,
                ..
            }
        ));
        assert!(matches!(
            &ops[6],
            WebOp::InjectKeyboardEvent {
                kind: WebKeyEventType::KeyDown,
                virtual_key_code: 0x20,
                ..
            }
        ));
    }

    #[test]
    fn inject_events_without_view_are_noops() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);
        p.borrow_mut().cleanup();

        p.borrow_mut()
            .inject_key_event(&KeyEvent::mouse(KeyCode::LeftMouse, true));
        p.borrow_mut()
            .inject_key_event(&key_event(KeyCode::KeyA, true, false, false, false));
        p.borrow_mut().inject_mouse_move_event(10, 20);
        p.borrow_mut().inject_mouse_wheel_event(-120);
        p.borrow_mut().execute_javascript("alert(1);", "");
        assert_eq!(p.borrow_mut().execute_javascript_with_result("1+1", ""), "");
        p.borrow_mut().load_url("http://www.google.com/");
        p.borrow_mut().stop();
        p.borrow_mut().reload(true);
        p.borrow_mut().go_back();
        p.borrow_mut().go_forward();
        p.borrow_mut().focus();
        p.borrow_mut().unfocus();
        assert_eq!(view_ops(&ctx, 0).len(), 3); // listener wiring + Destroy
    }

    #[test]
    fn mouse_move_and_wheel_forward_with_zero_horizontal() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);
        p.borrow_mut().inject_mouse_move_event(10, 20);
        p.borrow_mut().inject_mouse_wheel_event(-120);
        let ops = view_ops(&ctx, 0);
        assert_eq!(
            &ops[2..],
            &[
                WebOp::InjectMouseMove { x: 10, y: 20 },
                WebOp::InjectMouseWheel { dz: -120, dw: 0 },
            ]
        );
    }

    #[test]
    fn execute_javascript_records_and_returns_scripted_result() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);

        p.borrow_mut().execute_javascript("doThing()", "");
        p.borrow_mut().queue_js_result("42");
        assert_eq!(
            p.borrow_mut()
                .execute_javascript_with_result("getAnswer()", ""),
            "42"
        );
        // A second synchronous call without a queued result gives "".
        assert_eq!(
            p.borrow_mut()
                .execute_javascript_with_result("getAnswer()", ""),
            ""
        );
        let ops = view_ops(&ctx, 0);
        assert!(matches!(
            &ops[2],
            WebOp::ExecuteJavascript { script, frame_xpath }
                if script == "doThing()" && frame_xpath.is_empty()
        ));
        assert!(matches!(
            &ops[3],
            WebOp::ExecuteJavascriptWithResult { script, frame_xpath }
                if script == "getAnswer()" && frame_xpath.is_empty()
        ));
    }

    #[test]
    fn script_attribute_round_trips_through_the_listener() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);

        assert_eq!(p.borrow().script_object(), None);
        p.borrow_mut().set_script_object(Some("webGui"));
        assert_eq!(p.borrow().script_object(), Some("webGui".to_string()));
        p.borrow_mut().set_script_object(None);
        assert_eq!(p.borrow().script_object(), None);
    }

    #[test]
    fn listener_dispatches_nine_callbacks_with_exact_tuples() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);
        p.borrow_mut().set_script_object(Some("webGui"));

        {
            let mut p = p.borrow_mut();
            let listener = p.listener_mut().unwrap();
            listener.on_change_title("Title");
            listener.on_change_address_bar("http://a/");
            listener.on_change_tooltip("tip");
            listener.on_change_target_url("http://b/");
            listener.on_change_cursor(2);
            listener.on_begin_loading_frame(1, true, "http://a/");
            listener.on_fail_loading_frame(2, false, -3, "http://c/");
            listener.on_finish_loading_frame(3, true, "http://a/");
            listener.on_document_ready("http://a/");
        }

        let expected: Vec<ScriptCall> = vec![
            ScriptCall {
                callback: "onChangeTitle",
                args: vec![ScriptArg::Str("Title".to_string())],
                prefix: "PyAwesomiumProvider::onChangeTitle: ",
            },
            ScriptCall {
                callback: "onChangeAddressBar",
                args: vec![ScriptArg::Str("http://a/".to_string())],
                prefix: "PyAwesomiumProvider::onChangeAddressBar: ",
            },
            ScriptCall {
                callback: "onChangeTooltip",
                args: vec![ScriptArg::Str("tip".to_string())],
                prefix: "PyAwesomiumProvider::onChangeTooltip: ",
            },
            ScriptCall {
                callback: "onChangeTargetURL",
                args: vec![ScriptArg::Str("http://b/".to_string())],
                prefix: "PyAwesomiumProvider::onChangeTargetURL: ",
            },
            ScriptCall {
                callback: "onChangeCursor",
                args: vec![ScriptArg::Int(2)],
                prefix: "PyAwesomiumProvider::onChangeCursor: ",
            },
            ScriptCall {
                callback: "onBeginLoadingFrame",
                args: vec![
                    ScriptArg::Int(1),
                    ScriptArg::Bool(true),
                    ScriptArg::Str("http://a/".to_string()),
                ],
                prefix: "PyAwesomiumProvider::onBeginLoadingFrame: ",
            },
            ScriptCall {
                callback: "onFailLoadingFrame",
                args: vec![
                    ScriptArg::Int(2),
                    ScriptArg::Bool(false),
                    ScriptArg::Int(-3),
                    ScriptArg::Str("http://c/".to_string()),
                ],
                prefix: "PyAwesomiumProvider::onFailLoadingFrame: ",
            },
            ScriptCall {
                callback: "onFinishLoadingFrame",
                args: vec![
                    ScriptArg::Int(3),
                    ScriptArg::Bool(true),
                    ScriptArg::Str("http://a/".to_string()),
                ],
                prefix: "PyAwesomiumProvider::onFinishLoadingFrame: ",
            },
            ScriptCall {
                callback: "onDocumentReady",
                args: vec![ScriptArg::Str("http://a/".to_string())],
                prefix: "PyAwesomiumProvider::onDocumentReady: ",
            },
        ];
        assert_eq!(p.borrow().script_calls(), expected);
    }

    #[test]
    fn listener_without_object_dispatches_nothing() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 64, 48);
        // No script object attached: the if (pObject_) gate blocks all.
        {
            let mut p = p.borrow_mut();
            let listener = p.listener_mut().unwrap();
            listener.on_change_title("Title");
            listener.on_document_ready("http://a/");
        }
        assert!(p.borrow().script_calls().is_empty());
    }

    #[test]
    fn core_tick_pumps_update_and_refreshes_all_providers() {
        let mut ctx = ctx_with_exe_dll();
        let mut app = WebApp::new();
        assert!(app.init(&mut ctx, false));
        let p1 = provider(&mut ctx, 8, 8);
        let p2 = provider(&mut ctx, 16, 16);
        ctx.views[0].borrow_mut().surface.buffer[..1].copy_from_slice(&[1]);
        ctx.views[1].borrow_mut().surface.buffer[..1].copy_from_slice(&[2]);

        app.tick(&mut ctx);
        assert_eq!(
            p1.borrow()
                .texture
                .as_ref()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()[0],
            1
        );
        assert_eq!(
            p2.borrow()
                .texture
                .as_ref()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()[0],
            2
        );
        assert_eq!(ctx.core_ops.last(), Some(&WebCoreOp::Update));

        // Without an initialised core the tick returns immediately.
        let ops_before = ctx.core_ops.len();
        ctx.initialised = false;
        app.tick(&mut ctx);
        assert_eq!(ctx.core_ops.len(), ops_before);
    }

    #[test]
    fn core_fini_cleans_providers_shuts_down_and_frees_module() {
        let mut ctx = ctx_with_exe_dll();
        let p = provider(&mut ctx, 8, 8);
        p.borrow_mut().set_script_object(Some("webGui"));

        // Driven through the WebApp task face, as the app shutdown does.
        let mut app = WebApp::new();
        app.fini(&mut ctx);
        assert!(!ctx.initialised);
        assert!(!ctx.module_loaded);
        assert!(!ctx.interceptor_installed);
        assert_eq!(p.borrow().script_object(), None);
        assert!(!p.borrow().has_view());
        assert!(matches!(
            &ctx.core_ops[..],
            [
                WebCoreOp::LoadLibrary { .. },
                WebCoreOp::Initialize { .. },
                WebCoreOp::SetResourceInterceptor { installed: true },
                WebCoreOp::SetResourceInterceptor { installed: false },
                WebCoreOp::Shutdown,
                WebCoreOp::FreeLibrary,
            ]
        ));

        // A second fini is a no-op (guards are all false).
        let ops_before = ctx.core_ops.len();
        web_core_fini(&mut ctx);
        assert_eq!(ctx.core_ops.len(), ops_before);
    }

    #[test]
    fn web_app_task_gates_on_quiting_and_registers_in_web_group() {
        let mut ctx = WebCoreContext::new();
        let mut app = WebApp::new();
        assert!(app.registered());

        assert!(!app.init(&mut ctx, true));
        assert!(app.init(&mut ctx, false));

        // "Web" sits after "VOIP" (app.cpp group order).
        let groups = crate::voip::MAIN_LOOP_GROUPS;
        let web_index = groups.iter().position(|g| *g == "Web").unwrap();
        assert_eq!(groups[web_index - 1], "VOIP");
        assert_eq!(WEB_MAIN_LOOP_TASK, "Web/App");

        // The provider init stub is empty symmetry — it changes nothing.
        web_core_init(&mut ctx);
        assert!(!ctx.initialised);
        assert!(WebApp::default().registered());
    }

    #[test]
    fn registry_records_absent_sdk_and_gating() {
        assert_eq!(WEB_RENDER_TARGETS.len(), 1);
        let target = &WEB_RENDER_TARGETS[0];
        assert_eq!(target.name, "awesomium");
        assert!(!target.sdk_source_available);
        assert_eq!(
            target.sdk_files,
            &["awesomium.dll", "awesomium_process.exe"]
        );
        assert!(target.enabled_when.contains("BW_AWESOMIUM_SUPPORT"));

        // ENABLE_AWESOMIUM is 0 on _WIN64 regardless of the library flag,
        // so with the SDK absent the gate is closed everywhere.
        assert!(!awesomium_gate_open());
    }
}
