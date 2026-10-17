//! Wayland side: one full-width layer surface per output, both islands drawn into it.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dmsh_core::{
    Activities, ActivitySpec, BatteryView, Control, Controls, Hit, IslandKind, Level,
    MenuEntryView, MenuView, Placement, Priority, Privacy, Rect, SessionAction, Slider, Stage,
    Target, Theme, TrayIconView, View,
    activity::Island,
    config::{Config, ConfigError},
    ipc::{Event, Reply, Request, Response, StateDump},
};
use dmsh_render::{ImageSource, Renderer};
use dmsh_sources::tray::{self, TrayIcon};
use dmsh_sources::{Command as SourceCommand, Event as SourceEvent, Removal};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle, RegistrationToken, channel,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager,
        },
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_protocols::wp::{
    cursor_shape::v1::client::wp_cursor_shape_device_v1::{self, Shape},
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
    idle_inhibit::zv1::client::{zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1},
    viewporter::client::{wp_viewport, wp_viewporter},
};

/// Linux input event code of the primary mouse button.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Fractional scales arrive as multiples of 1/120.
const SCALE_DENOM: u32 = 120;

/// Activity used to report config errors in the island.
const CONFIG_ERROR_ID: &str = "config-error";

pub fn run(config_path: Option<PathBuf>) -> Result<()> {
    let started = Instant::now();
    let config_path = config_path.or_else(crate::config::default_path);
    let (config, config_error) = match crate::config::load(config_path.as_deref()) {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(e)),
    };
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor missing")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("wlr-layer-shell missing")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm missing")?;
    // Both optional: without them we fall back to integer buffer_scale.
    let fractional = globals.bind(&qh, 1..=1, FractionalManagerData).ok();
    let viewporter = globals.bind(&qh, 1..=1, ViewporterData).ok();
    let fractional = fractional.zip(viewporter);
    if fractional.is_none() {
        log::warn!("fractional scaling unavailable, using integer scale");
    }

    let mut event_loop: EventLoop<Shell> = EventLoop::try_new()?;
    let _socket = crate::ipc::listen(&event_loop.handle())?;

    let cursor_shape = CursorShapeManager::bind(&globals, &qh).ok();
    let idle_inhibit = globals.bind(&qh, 1..=1, IdleInhibitData).ok();
    if idle_inhibit.is_none() {
        log::warn!("compositor has no idle-inhibit; keep awake won't work");
    }
    let mut shell = Shell {
        seat_state: SeatState::new(&globals, &qh),
        pointer: None,
        keyboard: None,
        keyboard_focus: None,
        cursor_shape,
        cursor_device: None,
        renderer: Renderer::new(config.font.as_deref()),
        idle_inhibit,
        started: Some(started),
        keep_awake: None,
        battery: None,
        wifi: None,
        dnd: false,
        volume: None,
        brightness: None,
        privacy: Privacy::default(),
        images: crate::images::ImageResolver::new(),
        qh: qh.clone(),
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        fractional,
        theme: config.theme.clone(),
        shared_config: Arc::new(RwLock::new(config.clone())),
        config,
        config_path: config_path.clone(),
        reload_timer: None,
        hold_timer: None,
        islands: Vec::new(),
        activities: Activities::default(),
        views: Views::default(),
        focused_output: None,
        clock: None,
        date: None,
        loop_handle: event_loop.handle(),
        timer: None,
        sources: None,
        subscribers: Vec::new(),
        tray: Vec::new(),
        tray_items: Vec::new(),
        menu: None,
        menu_output: None,
    };
    if let Some(path) = &config_path
        && let Err(e) = crate::config::watch(&event_loop.handle(), path)
    {
        log::warn!("not watching {}: {e}", path.display());
    }

    // Sources run on their own thread and report back through this channel.
    let (source_tx, source_rx) = channel::channel::<SourceEvent>();
    event_loop
        .handle()
        .insert_source(source_rx, |event, _, shell| {
            if let channel::Event::Msg(event) = event {
                shell.on_source_event(event);
            }
        })
        .map_err(|e| anyhow::anyhow!("inserting sources channel: {e}"))?;
    match dmsh_sources::spawn(shell.shared_config.clone(), move |event| {
        let _ = source_tx.send(event);
    }) {
        Ok(handle) => shell.sources = Some(handle),
        Err(e) => log::error!("starting sources: {e}"),
    }
    if let Some(e) = config_error {
        shell.report_config_error(&e);
    }

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("inserting Wayland source: {e}"))?;
    event_loop.run(None, &mut shell, |_| {})?;
    Ok(())
}

pub struct Shell {
    config: Config,
    /// The same config, shared with the sources thread.
    shared_config: Arc<RwLock<Config>>,
    config_path: Option<PathBuf>,
    reload_timer: Option<RegistrationToken>,
    /// Ticks while a session menu option is held, to animate the fill and fire at the end.
    hold_timer: Option<RegistrationToken>,
    registry_state: RegistryState,
    seat_state: SeatState,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// Output whose islands have keyboard focus (while one is pinned open).
    keyboard_focus: Option<usize>,
    cursor_shape: Option<CursorShapeManager>,
    cursor_device: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    /// Fonts and glyph caches, shared by every output; also measures text for layout.
    renderer: Renderer,
    idle_inhibit: Option<zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1>,
    /// Process start, until the first frame is shown (for the startup-time budget).
    started: Option<Instant>,
    /// Present while "keep awake" is on; attached to an island surface, which is always
    /// visible, so the compositor (and idle daemons) treat the session as active.
    keep_awake: Option<zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1>,
    battery: Option<BatteryView>,
    /// Wi-Fi radio state; `None` without a Wi-Fi device.
    wifi: Option<bool>,
    /// Do not disturb.
    dnd: bool,
    volume: Option<Level>,
    brightness: Option<Level>,
    privacy: Privacy,
    images: crate::images::ImageResolver,
    qh: QueueHandle<Shell>,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    fractional: Option<(
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_viewporter::WpViewporter,
    )>,
    theme: Theme,
    islands: Vec<OutputSurface>,
    activities: Activities,
    /// What the islands currently show; outputs are redrawn when it changes.
    views: Views,
    /// Connector name of the focused output, from niri; `None` treats every output as
    /// focused.
    focused_output: Option<String>,
    clock: Option<String>,
    date: Option<String>,
    loop_handle: LoopHandle<'static, Shell>,
    /// Fires at the arbiter's next deadline (expiry or center→right move).
    timer: Option<RegistrationToken>,
    sources: Option<dmsh_sources::Handle>,
    /// IPC connections that asked for the event stream.
    subscribers: Vec<UnixStream>,
    tray: Vec<TrayIconView>,
    /// As the tray source sent them, to re-register icons after a renderer change.
    tray_items: Vec<tray::TrayItem>,
    menu: Option<OpenMenu>,
    /// Output whose tray was clicked last; a menu fetched for it opens there.
    menu_output: Option<usize>,
}

/// A tray menu shown on one output's right island.
struct OpenMenu {
    menu: tray::Menu,
    output: usize,
    /// Submenu ids from the top level to the level shown.
    path: Vec<i32>,
    /// First entry shown at that level.
    scroll: usize,
}

impl OpenMenu {
    fn view(&self) -> MenuView {
        let mut entries = &self.menu.entries;
        for id in &self.path {
            match entries.iter().find(|e| e.id == *id) {
                Some(e) => entries = &e.children,
                None => break,
            }
        }
        let mut shown: Vec<MenuEntryView> = Vec::new();
        for e in entries {
            // Apps often send doubled, leading or trailing separators.
            if e.separator && shown.last().is_none_or(|last| last.separator) {
                continue;
            }
            shown.push(MenuEntryView {
                id: e.id,
                label: e.label.clone(),
                enabled: e.enabled,
                separator: e.separator,
                toggle: e.toggle,
                submenu: !e.children.is_empty(),
            });
        }
        if shown.last().is_some_and(|last| last.separator) {
            shown.pop();
        }
        MenuView {
            title: self.menu.title.clone(),
            entries: shown,
            nested: !self.path.is_empty(),
            offset: self.scroll,
        }
    }

    /// The entry with `id` at the level shown.
    fn entry(&self, id: i32) -> Option<&tray::MenuEntry> {
        let mut entries = &self.menu.entries;
        for p in &self.path {
            entries = &entries.iter().find(|e| e.id == *p)?.children;
        }
        entries.iter().find(|e| e.id == id)
    }
}

/// Transient center activities show only on the focused output, so outputs can differ.
#[derive(Clone, Default, PartialEq)]
struct Views {
    focused: View,
    other: View,
}

/// The layer surface holding both islands on one output.
struct OutputSurface {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    scaling: Option<(
        wp_fractional_scale_v1::WpFractionalScaleV1,
        wp_viewport::WpViewport,
    )>,
    /// Logical size from the last configure; `None` until the first one.
    size: Option<(u32, u32)>,
    /// Canvas height last asked of the compositor.
    requested_height: u32,
    /// A resize was requested and its configure hasn't arrived yet.
    resize_pending: bool,
    /// Output scale in 1/120ths.
    scale: u32,
    /// Per output, so a collapse on one output can return memory without affecting others.
    pool: SlotPool,
    /// Animated island state on this output.
    stage: Stage,
    hover: Option<IslandKind>,
    /// Opened from a keybind: stays expanded and takes the keyboard until closed.
    pinned: Option<IslandKind>,
    /// First notification row shown in the expanded right island.
    right_scroll: usize,
    /// Touchpad scroll distance not yet turned into whole rows.
    scroll_accum: f64,
    /// The session menu, open in the expanded right island.
    session: SessionMenu,
    /// The volume or brightness slider open in the expanded right island.
    slider: Option<Slider>,
    /// A slider being dragged, and its track in surface coordinates.
    drag: Option<(Slider, Rect)>,
    /// The volume button is pressed: a short press opens the slider, a long one mutes.
    /// Holds the mute timer, `None` once it fired.
    volume_press: Option<Option<RegistrationToken>>,
    /// A frame callback is outstanding; the next draw happens when it fires.
    frame_pending: bool,
    /// Timestamp of the last animation frame, for frame-rate independent springs.
    last_frame_ms: Option<u32>,
}

/// The session menu on one output, and the option being held down, since when.
#[derive(Default)]
struct SessionMenu {
    open: bool,
    hold: Option<(SessionAction, Instant)>,
}

/// How often the hold fill is redrawn.
const HOLD_TICK: Duration = Duration::from_millis(16);
/// Holding the volume button this long mutes instead of opening the slider.
const MUTE_HOLD: Duration = Duration::from_millis(450);
/// One arrow key press or wheel notch on a slider.
const SLIDER_STEP: f32 = 0.05;

impl Drop for OutputSurface {
    fn drop(&mut self) {
        if let Some((fractional, viewport)) = &self.scaling {
            fractional.destroy();
            viewport.destroy();
        }
    }
}

impl Shell {
    pub fn handle(&mut self, request: Request) -> Response {
        self.refresh();
        let now = Instant::now();
        let reply = match request {
            Request::Push(mut spec) => {
                if let Some(image) = spec.image.take() {
                    spec.image = self.images.resolve(&image, &mut self.renderer);
                }
                self.activities.push(spec, now).map(|()| Reply::Done)
            }
            Request::Dismiss { id } => {
                let existed = self.activities.dismiss(&id);
                if existed {
                    self.notify_removed(id, Removal::Dismissed);
                }
                Ok(Reply::Dismissed { existed })
            }
            Request::Clear { island } => {
                let removed = match island {
                    Some(island) => self.activities.clear_island(island, now),
                    None => self.activities.clear(),
                };
                for id in removed {
                    self.notify_removed(id, Removal::Dismissed);
                }
                Ok(Reply::Done)
            }
            Request::Invoke { id, key } => match self.activities.action(&id, &key) {
                None => Err(format!("`{id}` has no action `{key}`")),
                Some(keep) => {
                    self.broadcast(&Event::ActionInvoked {
                        id: id.clone(),
                        key: key.clone(),
                    });
                    if let Some(sources) = &self.sources {
                        sources.send(SourceCommand::Invoked {
                            id: id.clone(),
                            key,
                        });
                    }
                    if !keep && self.activities.dismiss(&id) {
                        self.notify_removed(id, Removal::Dismissed);
                    }
                    Ok(Reply::Done)
                }
            },
            // The IPC server registers the connection; this just acknowledges it.
            Request::Subscribe => Ok(Reply::Subscribed),
            Request::State => Ok(Reply::State(Box::new(StateDump {
                view: self.views.focused.clone(),
                focused_output: self.focused_output.clone(),
                activities: self.activities.states(now),
            }))),
            Request::Reload => self.reload().map(|()| Reply::Done),
            Request::Toggle { island } => self.toggle(island).map(|()| Reply::Done),
            Request::Control { control, state } => {
                self.set_control(control, state).map(|()| Reply::Done)
            }
            Request::Version => Ok(Reply::Version {
                version: env!("CARGO_PKG_VERSION").into(),
            }),
        };
        self.refresh();
        match reply {
            Ok(reply) => Response::Ok(reply),
            Err(e) => Response::Error(e),
        }
    }

    /// Drops expired activities, redraws if the view changed, and re-arms the timer for
    /// the next deadline. Nothing runs between deadlines, so idle CPU stays at zero.
    fn refresh(&mut self) {
        let now = Instant::now();
        for id in self.activities.expire(now) {
            self.notify_removed(id, Removal::Expired);
        }
        let view_for = |focused| View {
            clock: self.clock.clone(),
            date: self.date.clone(),
            tray: self.tray.clone(),
            battery: self.battery,
            controls: Controls {
                wifi: self.wifi,
                keep_awake: self.keep_awake.is_some(),
                dnd: self.dnd,
                volume: self.volume,
                brightness: self.brightness,
                ..Controls::default()
            },
            privacy: self.privacy,
            ..self.activities.view_for(now, focused)
        };
        let views = Views {
            focused: view_for(true),
            other: view_for(false),
        };
        if views != self.views {
            self.views = views;
            self.update_all();
        }

        if let Some(token) = self.timer.take() {
            self.loop_handle.remove(token);
        }
        if let Some(deadline) = self.activities.next_deadline(now) {
            let timer = Timer::from_deadline(deadline);
            let token = self.loop_handle.insert_source(timer, |_, _, shell| {
                shell.timer = None;
                shell.refresh();
                TimeoutAction::Drop
            });
            match token {
                Ok(token) => self.timer = Some(token),
                Err(e) => log::error!("arming timer: {e}"),
            }
        }
    }

    fn on_source_event(&mut self, event: SourceEvent) {
        match event {
            SourceEvent::Request(request) => {
                if let Response::Error(e) = self.handle(request) {
                    log::warn!("source request failed: {e}");
                }
            }
            SourceEvent::FocusedOutput(output) => {
                if output != self.focused_output {
                    self.focused_output = output;
                    self.update_all();
                }
            }
            SourceEvent::Tray(items) => self.set_tray(items),
            SourceEvent::Battery(battery) => {
                self.battery = Some(battery);
                self.refresh();
            }
            SourceEvent::Image {
                key,
                width,
                height,
                rgba,
                icon,
            } => {
                let source = ImageSource::Rgba {
                    width,
                    height,
                    data: rgba,
                };
                self.renderer.set_image(&key, source, icon);
            }
            SourceEvent::Wifi(on) => {
                self.wifi = Some(on);
                self.refresh();
            }
            // While a slider is dragged it shows where the pointer is; the system's level
            // catches up (and may lag a few steps behind).
            SourceEvent::Volume(level) => {
                if !self.dragging(Slider::Volume) {
                    self.volume = Some(level);
                    self.refresh();
                }
            }
            SourceEvent::Privacy(sensor, on) => {
                self.privacy.set(sensor, on);
                self.refresh();
            }
            SourceEvent::Brightness(level) => {
                if !self.dragging(Slider::Brightness) {
                    self.brightness = Some(level);
                    self.refresh();
                }
            }
            SourceEvent::Menu(menu) => {
                self.menu = menu.map(|menu| OpenMenu {
                    menu,
                    output: self.menu_output.unwrap_or(0),
                    path: Vec::new(),
                    scroll: 0,
                });
                self.update_all();
            }
            SourceEvent::Clock { time, date } => {
                self.clock = Some(time);
                self.date = Some(date);
                self.refresh();
            }
        }
    }

    /// Tells subscribers and the owning source (e.g. to emit `NotificationClosed`).
    fn notify_removed(&mut self, id: String, reason: Removal) {
        self.retain_images();
        self.broadcast(&Event::Removed {
            id: id.clone(),
            reason,
        });
        if let Some(sources) = &self.sources {
            sources.send(SourceCommand::Removed { id, reason });
        }
    }

    pub fn add_subscriber(&mut self, stream: UnixStream) {
        self.subscribers.push(stream);
    }

    /// Sends an event to every subscriber; ones that are gone or not reading are dropped.
    fn broadcast(&mut self, event: &Event) {
        if self.subscribers.is_empty() {
            return;
        }
        let mut line = serde_json::to_vec(event).expect("events always serialize");
        line.push(b'\n');
        self.subscribers.retain(|s| (&*s).write_all(&line).is_ok());
    }

    /// Performs a click on an island.
    /// Registers tray icons with the renderer and shows them.
    fn set_tray(&mut self, items: Vec<tray::TrayItem>) {
        self.tray_items = items.clone();
        self.tray = items
            .into_iter()
            .map(|item| {
                let image = item.icon.map(|icon| {
                    let (key, source) = match icon {
                        TrayIcon::Pixels {
                            width,
                            height,
                            rgba,
                            hash,
                        } => (
                            format!("px:{}:{hash:x}", item.id),
                            ImageSource::Rgba {
                                width,
                                height,
                                data: rgba,
                            },
                        ),
                        TrayIcon::File(path) => {
                            (format!("file:{}", path.display()), ImageSource::File(path))
                        }
                    };
                    self.renderer.set_image(&key, source, true);
                    key
                });
                TrayIconView {
                    id: item.id,
                    title: item.title,
                    image,
                    needs_attention: item.needs_attention,
                }
            })
            .collect();
        self.retain_images();
        self.refresh();
    }

    /// Scrolls an output's expanded notification list by `rows` (negative = newer).
    fn scroll_right(&mut self, index: usize, rows: i32) {
        // An open menu replaces the list, so it gets the scrolling.
        if let Some(menu) = self.menu.as_mut().filter(|m| m.output == index) {
            let len = menu.view().entries.len();
            let max = len.saturating_sub(dmsh_core::MENU_ROWS) as i64;
            let scroll = (menu.scroll as i64 + i64::from(rows)).clamp(0, max) as usize;
            if scroll != menu.scroll {
                menu.scroll = scroll;
                self.update_stage(index);
            }
            return;
        }
        let items = self
            .views
            .focused
            .right
            .as_ref()
            .map_or(0, |r| r.items.len());
        let max = items.saturating_sub(dmsh_core::LIST_ROWS);
        let island = &mut self.islands[index];
        let scroll = (island.right_scroll as i64 + i64::from(rows)).clamp(0, max as i64) as usize;
        if scroll != island.right_scroll {
            island.right_scroll = scroll;
            self.update_stage(index);
        }
    }

    /// Drops images nothing refers to any more (tray icons, activity pictures).
    fn retain_images(&mut self) {
        let mut keys: Vec<String> = self.tray.iter().filter_map(|t| t.image.clone()).collect();
        keys.extend(self.activities.image_keys().map(str::to_owned));
        self.renderer
            .retain_images(|k| keys.iter().any(|key| key == k));
    }

    /// Sets a quick control, or flips it when `state` is `None`.
    fn set_control(&mut self, control: Control, state: Option<bool>) -> Result<(), String> {
        match control {
            Control::Wifi => {
                let current = self.wifi.ok_or("no Wi-Fi device")?;
                if let Some(sources) = &self.sources {
                    // The UI follows NetworkManager's change signal.
                    sources.send(SourceCommand::SetWifi(state.unwrap_or(!current)));
                }
            }
            Control::KeepAwake => {
                let on = state.unwrap_or(self.keep_awake.is_none());
                self.set_keep_awake(on)?;
                self.refresh();
            }
            Control::Dnd => {
                self.dnd = state.unwrap_or(!self.dnd);
                self.activities.set_quiet(self.dnd);
                self.refresh();
            }
        }
        Ok(())
    }

    fn set_keep_awake(&mut self, on: bool) -> Result<(), String> {
        if let Some(inhibitor) = self.keep_awake.take() {
            inhibitor.destroy();
        }
        if !on {
            return Ok(());
        }
        let manager = self
            .idle_inhibit
            .as_ref()
            .ok_or("the compositor doesn't support idle inhibition")?;
        let island = self.islands.first().ok_or("no outputs with islands")?;
        let surface = island.layer.wl_surface();
        self.keep_awake = Some(manager.create_inhibitor(surface, &self.qh, IdleInhibitData));
        Ok(())
    }

    /// Re-attaches keep awake after the islands changed (its surface may be gone).
    fn reattach_keep_awake(&mut self) {
        if self.keep_awake.is_some()
            && let Err(e) = self.set_keep_awake(true)
        {
            log::warn!("keep awake: {e}");
        }
    }

    /// The output niri reports as focused (else the first one).
    fn focused_index(&self) -> Option<usize> {
        let by_name = self.islands.iter().position(|i| {
            let name = self.output_state.info(&i.output).and_then(|info| info.name);
            name.is_some() && name == self.focused_output
        });
        by_name.or((!self.islands.is_empty()).then_some(0))
    }

    /// Opens an island on the focused output with keyboard focus, or closes it.
    fn toggle(&mut self, island: Island) -> Result<(), String> {
        let kind = match island {
            Island::Center => IslandKind::Center,
            Island::Right => IslandKind::Right,
        };
        let index = self.focused_index().ok_or("no outputs with islands")?;
        if self.islands[index].pinned == Some(kind) {
            self.unpin(index);
            return Ok(());
        }
        for i in 0..self.islands.len() {
            if i != index && self.islands[i].pinned.is_some() {
                self.unpin(i);
            }
        }
        let island = &mut self.islands[index];
        island.pinned = Some(kind);
        island
            .layer
            .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        island.stage.set_keyboard(Some(kind));
        self.update_stage(index);
        Ok(())
    }

    fn unpin(&mut self, index: usize) {
        let island = &mut self.islands[index];
        if island.pinned.take().is_none() {
            return;
        }
        island
            .layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        island.stage.set_keyboard(None);
        if self.menu.as_ref().is_some_and(|m| m.output == index) {
            self.menu = None;
        }
        self.update_stage(index);
    }

    /// Keyboard navigation of a pinned island.
    fn on_key(&mut self, index: usize, key: Keysym) {
        let island = &mut self.islands[index];
        if island.pinned.is_none() {
            return;
        }
        let selected = island.stage.selected_target();
        // Left and right move a selected slider instead of the selection.
        if let Some(Target::Slider(slider)) = selected {
            let delta = match key {
                Keysym::Left | Keysym::h => -SLIDER_STEP,
                Keysym::Right | Keysym::l => SLIDER_STEP,
                _ => 0.0,
            };
            if delta != 0.0 {
                self.step_level(slider, delta);
                return;
            }
        }
        match key {
            Keysym::Escape => {
                if self.menu.as_ref().is_some_and(|m| m.output == index) {
                    self.close_menu();
                } else if island.session.open {
                    self.click(index, Target::SessionMenu(false), BTN_LEFT);
                } else if island.slider.is_some() {
                    self.click(index, Target::CloseSlider, BTN_LEFT);
                } else {
                    self.unpin(index);
                }
            }
            Keysym::Up | Keysym::Left | Keysym::k | Keysym::h | Keysym::ISO_Left_Tab => {
                self.move_selection(index, -1)
            }
            Keysym::Down | Keysym::Right | Keysym::j | Keysym::l | Keysym::Tab => {
                self.move_selection(index, 1)
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                if let Some(target) = selected {
                    self.activate(index, target, BTN_LEFT);
                }
            }
            Keysym::Menu | Keysym::m => {
                if let Some(target @ Target::Tray { .. }) = selected {
                    self.activate(index, target, BTN_RIGHT);
                }
            }
            Keysym::Delete | Keysym::BackSpace | Keysym::d => {
                if let Some(Target::Invoke { id, .. } | Target::Dismiss { id }) = selected
                    && let Response::Error(e) = self.handle(Request::Dismiss { id })
                {
                    log::warn!("dismiss failed: {e}");
                }
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, index: usize, delta: i32) {
        let island = &mut self.islands[index];
        if island.stage.move_selection(delta) && !island.frame_pending {
            self.draw(index);
        }
    }

    /// Activates a target from the keyboard. Things that hand focus to an app (opening a
    /// notification, a tray app, a menu entry) also close the pinned island.
    fn activate(&mut self, index: usize, target: Target, button: u32) {
        let hands_off = match &target {
            Target::Invoke { .. } => true,
            Target::Tray { .. } => button == BTN_LEFT,
            Target::MenuItem { id } => self
                .menu
                .as_ref()
                .and_then(|m| m.entry(*id))
                .is_some_and(|e| e.children.is_empty()),
            _ => false,
        };
        self.click(index, target, button);
        if hands_off {
            self.unpin(index);
        }
    }

    /// Reloads the config shortly; repeated calls push the reload back.
    pub fn schedule_reload(&mut self) {
        if let Some(token) = self.reload_timer.take() {
            self.loop_handle.remove(token);
        }
        let timer = Timer::from_duration(crate::config::RELOAD_DELAY);
        let token = self.loop_handle.insert_source(timer, |_, _, shell| {
            shell.reload_timer = None;
            let _ = shell.reload();
            TimeoutAction::Drop
        });
        self.reload_timer = token.ok();
    }

    /// Re-reads the config. On error the current config stays and the error is shown.
    fn reload(&mut self) -> Result<(), String> {
        match crate::config::load(self.config_path.as_deref()) {
            Ok(config) => {
                log::info!("config reloaded");
                self.apply_config(config);
                if self.activities.dismiss(CONFIG_ERROR_ID) {
                    self.refresh();
                }
                Ok(())
            }
            Err(e) => {
                self.report_config_error(&e);
                Err(format!("config: {e}"))
            }
        }
    }

    fn report_config_error(&mut self, e: &ConfigError) {
        log::error!("config: {e}");
        let spec = ActivitySpec {
            body: Some(e.to_string()),
            icon: Some("alert".into()),
            placement: Placement::Center,
            priority: Priority::High,
            ttl_ms: Some(10_000),
            ..ActivitySpec::new(CONFIG_ERROR_ID, "Config error")
        };
        if let Response::Error(e) = self.handle(Request::Push(spec)) {
            log::warn!("showing config error: {e}");
        }
    }

    fn apply_config(&mut self, config: Config) {
        let font_changed = config.font != self.config.font;
        let strip_changed = config.theme.strip_height != self.theme.strip_height;
        self.theme = config.theme.clone();
        *self
            .shared_config
            .write()
            .unwrap_or_else(|e| e.into_inner()) = config.clone();
        self.config = config;
        if let Some(sources) = &self.sources {
            sources.config_changed();
        }
        if font_changed {
            self.renderer = Renderer::new(self.config.font.as_deref());
            self.set_tray(self.tray_items.clone());
        }
        if strip_changed {
            for island in &self.islands {
                island
                    .layer
                    .set_exclusive_zone(self.theme.strip_height as i32);
            }
        }
        // Outputs that are no longer wanted go away; newly wanted ones get islands.
        let wanted = |shell: &Self, output: &wl_output::WlOutput| {
            let name = shell.output_state.info(output).and_then(|i| i.name);
            shell.config.outputs.wants(name.as_deref())
        };
        let keep: Vec<bool> = self
            .islands
            .iter()
            .map(|i| wanted(self, &i.output))
            .collect();
        let mut keep = keep.into_iter();
        self.islands.retain(|_| keep.next().unwrap_or(true));
        self.menu = None;
        for output in self.output_state.outputs() {
            let present = self.islands.iter().any(|i| i.output == output);
            if !present && wanted(self, &output) {
                let qh = self.qh.clone();
                self.add_output(&qh, output);
            }
        }
        self.reattach_keep_awake();
        self.update_all();
    }

    fn tray_command(&self, command: tray::Command) {
        if let Some(sources) = &self.sources {
            sources.send(SourceCommand::Tray(command));
        }
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.update_all();
        }
    }

    /// Performs a click on an island of output `index`.
    fn click(&mut self, index: usize, target: Target, button: u32) {
        let request = match target {
            Target::Tray { id } => {
                self.menu_output = Some(index);
                let item = id;
                self.tray_command(match button {
                    BTN_RIGHT => tray::Command::OpenMenu { item },
                    BTN_MIDDLE => tray::Command::SecondaryActivate { item },
                    _ => tray::Command::Activate { item },
                });
                return;
            }
            Target::MenuItem { id } => {
                let Some(menu) = &mut self.menu else { return };
                match menu.entry(id) {
                    Some(entry) if !entry.children.is_empty() => {
                        menu.path.push(id);
                        menu.scroll = 0;
                    }
                    Some(_) => {
                        let item = menu.menu.item.clone();
                        self.menu = None;
                        self.tray_command(tray::Command::MenuClick { item, entry: id });
                    }
                    None => return,
                }
                self.update_all();
                return;
            }
            Target::MenuBack => {
                if let Some(menu) = &mut self.menu {
                    menu.path.pop();
                    menu.scroll = 0;
                    self.update_all();
                }
                return;
            }
            _ if button != BTN_LEFT => return,
            Target::Scroll(delta) => {
                self.scroll_right(index, delta);
                return;
            }
            Target::Control(control) => {
                if let Err(e) = self.set_control(control, None) {
                    log::warn!("{e}");
                }
                return;
            }
            Target::SessionMenu(open) => {
                self.islands[index].session = SessionMenu { open, hold: None };
                self.update_stage(index);
                // Keep the keyboard selection on the same spot: the first option when
                // opening, the power button when going back.
                let select = match open {
                    true => Target::Session(SessionAction::Poweroff),
                    false => Target::SessionMenu(true),
                };
                let island = &mut self.islands[index];
                if island.stage.select(&select) && !island.frame_pending {
                    self.draw(index);
                }
                return;
            }
            Target::Session(action) => {
                self.start_hold(index, action);
                return;
            }
            Target::Level(Slider::Volume) => {
                self.press_volume(index);
                return;
            }
            Target::Level(slider) => {
                self.open_slider(index, Some(slider));
                return;
            }
            Target::CloseSlider => {
                self.open_slider(index, None);
                return;
            }
            // Pressing a track starts a drag; see the pointer handler.
            Target::Slider(_) => return,
            Target::Invoke { id, key } => Request::Invoke { id, key },
            Target::Dismiss { id } => Request::Dismiss { id },
            Target::ClearRight => Request::Clear {
                island: Some(Island::Right),
            },
        };
        if let Response::Error(e) = self.handle(request) {
            log::warn!("click failed: {e}");
        }
    }

    /// Starts filling a session menu option; it runs if still held when full.
    fn start_hold(&mut self, index: usize, action: SessionAction) {
        self.islands[index].session.hold = Some((action, Instant::now()));
        self.update_stage(index);
        if self.hold_timer.is_some() {
            return;
        }
        let timer = Timer::from_duration(HOLD_TICK);
        let token = self.loop_handle.insert_source(timer, |_, _, shell| {
            if shell.tick_holds() {
                TimeoutAction::ToDuration(HOLD_TICK)
            } else {
                shell.hold_timer = None;
                TimeoutAction::Drop
            }
        });
        self.hold_timer = token.ok();
    }

    fn dragging(&self, slider: Slider) -> bool {
        self.islands
            .iter()
            .any(|i| i.drag.is_some_and(|(s, _)| s == slider))
    }

    /// Opens a slider in place of the volume and brightness buttons, or closes it, keeping
    /// the keyboard selection on the matching spot.
    fn open_slider(&mut self, index: usize, slider: Option<Slider>) {
        let island = &mut self.islands[index];
        let select = match (slider, island.slider) {
            (Some(s), _) => Target::Slider(s),
            (None, Some(s)) => Target::Level(s),
            (None, None) => return,
        };
        island.slider = slider;
        island.drag = None;
        self.update_stage(index);
        let island = &mut self.islands[index];
        if island.stage.select(&select) && !island.frame_pending {
            self.draw(index);
        }
    }

    /// The volume button went down: mute if it's still held after a moment.
    fn press_volume(&mut self, index: usize) {
        self.cancel_volume_press(index);
        let timer = Timer::from_duration(MUTE_HOLD);
        let token = self.loop_handle.insert_source(timer, |_, _, shell| {
            for island in &mut shell.islands {
                if let Some(timer @ Some(_)) = &mut island.volume_press {
                    *timer = None;
                }
            }
            shell.toggle_mute();
            TimeoutAction::Drop
        });
        self.islands[index].volume_press = Some(token.ok());
    }

    fn cancel_volume_press(&mut self, index: usize) {
        if let Some(Some(token)) = self.islands[index].volume_press.take() {
            self.loop_handle.remove(token);
        }
    }

    /// The mouse button or Enter came up: finishes a press on the volume button (a short
    /// one opens the slider), a drag, or cancels a session hold.
    fn release(&mut self, index: usize) {
        self.cancel_hold(index);
        self.islands[index].drag = None;
        // A short press opens the slider; after a long one (it muted) nothing more happens.
        if let Some(Some(token)) = self.islands[index].volume_press.take() {
            self.loop_handle.remove(token);
            self.open_slider(index, Some(Slider::Volume));
        }
    }

    fn toggle_mute(&mut self) {
        let Some(volume) = &mut self.volume else {
            return;
        };
        volume.muted = !volume.muted;
        if let Some(sources) = &self.sources {
            sources.send(SourceCommand::SetVolume(
                dmsh_sources::volume::SetVolume::ToggleMute,
            ));
        }
        self.refresh();
    }

    /// Sets a level right away in the islands and asks the system for it.
    fn set_level(&mut self, slider: Slider, value: f32) {
        let value = value.clamp(0.0, 1.0);
        let (level, command) = match slider {
            Slider::Volume => (
                &mut self.volume,
                SourceCommand::SetVolume(dmsh_sources::volume::SetVolume::Level(value)),
            ),
            Slider::Brightness => (&mut self.brightness, SourceCommand::SetBrightness(value)),
        };
        let Some(level) = level else { return };
        let changed = level.value != value || level.muted;
        // Setting the volume also unmutes it.
        *level = Level {
            value,
            muted: false,
        };
        if changed {
            if let Some(sources) = &self.sources {
                sources.send(command);
            }
            self.refresh();
        }
    }

    fn step_level(&mut self, slider: Slider, delta: f32) {
        let current = match slider {
            Slider::Volume => self.volume,
            Slider::Brightness => self.brightness,
        };
        if let Some(level) = current {
            // Steps land on multiples of the step, so 47% + 5% is 50%.
            let steps = (level.value.min(1.0) / SLIDER_STEP).round() + delta / SLIDER_STEP;
            self.set_level(slider, steps * SLIDER_STEP);
        }
    }

    /// Let go before the option filled up: nothing happens.
    fn cancel_hold(&mut self, index: usize) {
        if self.islands[index].session.hold.take().is_some() {
            self.update_stage(index);
        }
    }

    /// Redraws held options and runs the ones held long enough; returns whether any
    /// are still held.
    fn tick_holds(&mut self) -> bool {
        let mut holding = false;
        for index in 0..self.islands.len() {
            let Some((action, since)) = self.islands[index].session.hold else {
                continue;
            };
            if since.elapsed() >= self.config.session.hold {
                self.islands[index].session = SessionMenu::default();
                self.unpin(index);
                self.update_stage(index);
                self.run_session(action);
            } else {
                holding = true;
                self.update_stage(index);
            }
        }
        holding
    }

    fn run_session(&self, action: SessionAction) {
        let command = self.config.session.command(action).to_owned();
        log::info!("{}: {command}", action.label());
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .spawn();
        match child {
            // Reap it off the main thread; logout and poweroff may take a while to return.
            Ok(mut child) => {
                std::thread::spawn(move || match child.wait() {
                    Ok(status) if !status.success() => {
                        log::warn!("`{command}` exited with {status}")
                    }
                    Err(e) => log::warn!("`{command}`: {e}"),
                    Ok(_) => {}
                });
            }
            Err(e) => log::warn!("running `{command}`: {e}"),
        }
    }

    fn set_pointer(&mut self, index: usize, pointer: Option<(f32, f32)>) {
        let island = &mut self.islands[index];
        if island.stage.set_pointer(pointer) && !island.frame_pending {
            self.draw(index);
        }
    }

    fn update_all(&mut self) {
        for i in 0..self.islands.len() {
            self.update_stage(i);
        }
    }

    /// Points an output's islands at its current view and hover state, then draws (or lets
    /// the running animation pick it up on its next frame).
    fn update_stage(&mut self, index: usize) {
        let name = self
            .output_state
            .info(&self.islands[index].output)
            .and_then(|info| info.name);
        let focused = match (&self.focused_output, &name) {
            (Some(focused), Some(name)) => focused == name,
            _ => true,
        };
        let mut view = if focused {
            self.views.focused.clone()
        } else {
            self.views.other.clone()
        };
        let island = &mut self.islands[index];
        if island.pinned.or(island.hover) != Some(IslandKind::Right) {
            island.right_scroll = 0;
            island.session = SessionMenu::default();
            island.slider = None;
            island.drag = None;
        }
        view.right_scroll = island.right_scroll;
        view.controls.session_open = island.session.open;
        view.controls.slider = island.slider;
        view.controls.session_hold = island.session.hold.map(|(action, since)| {
            let hold = self.config.session.hold.as_secs_f32().max(0.001);
            (action, since.elapsed().as_secs_f32() / hold)
        });
        view.menu = self
            .menu
            .as_ref()
            .filter(|m| m.output == index)
            .map(OpenMenu::view);
        let island = &mut self.islands[index];
        let Some((width, _)) = island.size else {
            return;
        };
        island.stage.update(
            width as f32,
            &view,
            island.pinned.or(island.hover),
            &self.theme,
            &mut self.renderer,
        );
        if !island.frame_pending {
            self.draw(index);
        }
    }

    /// Advances an output's animation by the time since its last frame and draws it.
    fn on_frame(&mut self, index: usize, time_ms: u32) {
        let island = &mut self.islands[index];
        island.frame_pending = false;
        let dt = island.last_frame_ms.map_or(1.0 / 120.0, |last| {
            time_ms.wrapping_sub(last) as f32 / 1000.0
        });
        // A long gap (output asleep, compositor busy) shouldn't fling the springs.
        island.stage.step(dt.clamp(0.0, 1.0 / 20.0));
        island.last_frame_ms = Some(time_ms);
        self.draw(index);
        let island = &mut self.islands[index];
        if !island.stage.animating() {
            island.last_frame_ms = None;
        }
    }

    fn set_hover(&mut self, index: usize, hover: Option<IslandKind>) {
        if self.islands[index].hover != hover {
            self.islands[index].hover = hover;
            self.update_stage(index);
        }
    }

    fn add_output(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let name = self.output_state.info(&output).and_then(|i| i.name);
        if !self.config.outputs.wants(name.as_deref()) {
            log::info!(
                "no islands on {}",
                name.as_deref().unwrap_or("unnamed output")
            );
            return;
        }
        let surface = self.compositor.create_surface(qh);
        let scaling = self.fractional.as_ref().map(|(manager, viewporter)| {
            let fractional =
                manager.get_fractional_scale(&surface, qh, SurfaceData(surface.clone()));
            let viewport = viewporter.get_viewport(&surface, qh, ViewportData);
            (fractional, viewport)
        });
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Top,
            Some("dmsh"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, self.theme.strip_height);
        layer.set_exclusive_zone(self.theme.strip_height as i32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Empty input region until the first draw sets the pill shapes.
        if let Ok(region) = Region::new(&self.compositor) {
            layer
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
        layer.commit();

        let scale = self
            .output_state
            .info(&output)
            .map_or(1, |info| info.scale_factor.max(1) as u32)
            * SCALE_DENOM;
        let pool = match SlotPool::new(1 << 20, &self.shm) {
            Ok(pool) => pool,
            Err(e) => {
                log::error!("creating buffer pool: {e}");
                return;
            }
        };
        self.islands.push(OutputSurface {
            output,
            layer,
            scaling,
            size: None,
            requested_height: self.theme.strip_height,
            resize_pending: false,
            scale,
            pool,
            stage: Stage::default(),
            hover: None,
            pinned: None,
            right_scroll: 0,
            scroll_accum: 0.0,
            session: SessionMenu::default(),
            slider: None,
            drag: None,
            volume_press: None,
            frame_pending: false,
            last_frame_ms: None,
        });
    }

    fn island_mut(&mut self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.islands
            .iter()
            .position(|i| i.layer.wl_surface() == surface)
    }

    fn draw(&mut self, index: usize) {
        let island = &mut self.islands[index];
        let Some((width, height)) = island.size else {
            return;
        };
        if island.resize_pending {
            return; // The configure for the new size will redraw.
        }

        // The canvas is only as tall as its content: the strip while collapsed, taller
        // while something floats over windows. The reserved strip never changes. It is
        // sized for both the current and target state, so it grows before an expansion
        // animates and shrinks after a collapse finishes.
        let needed = island.stage.canvas_height(&self.theme);
        if needed != island.requested_height {
            island.layer.set_size(0, needed);
            island.layer.commit();
            island.requested_height = needed;
            island.resize_pending = true;
            return;
        }

        // Rounding rule from the fractional-scale protocol: half away from zero.
        let to_buffer = |v: u32| ((v * island.scale + SCALE_DENOM / 2) / SCALE_DENOM).max(1);
        let (bw, bh) = (to_buffer(width), to_buffer(height));

        // Pools only grow. Steady state needs two buffers (one on screen, one drawing), so
        // a pool much larger than that is left over from an expansion: swap in a small one
        // to return the memory. Buffers the compositor still holds stay valid on its side.
        let len = (bw * bh * 4) as usize;
        if island.pool.len() > 3 * len {
            match SlotPool::new(2 * len, &self.shm) {
                Ok(pool) => island.pool = pool,
                Err(e) => log::warn!("shrinking buffer pool: {e}"),
            }
        }

        let (buffer, canvas) = match island.pool.create_buffer(
            bw as i32,
            bh as i32,
            bw as i32 * 4,
            wl_shm::Format::Argb8888,
        ) {
            Ok(b) => b,
            Err(e) => {
                log::error!("creating buffer: {e}");
                return;
            }
        };
        // Pixels per logical unit, after buffer rounding.
        let px_scale = bw as f32 / width as f32;
        let scene = island.stage.scene(&self.theme);
        if !self
            .renderer
            .render_argb8888(&scene, px_scale, canvas, bw, bh)
        {
            log::error!("invalid buffer size {bw}x{bh}");
            return;
        }

        let surface = island.layer.wl_surface();
        if let Ok(region) = Region::new(&self.compositor) {
            for rect in &scene.islands {
                let (x, y, w, h) = rect.round_out();
                region.add(x, y, w, h);
            }
            surface.set_input_region(Some(region.wl_region()));
        }
        match &island.scaling {
            Some((_, viewport)) => {
                surface.set_buffer_scale(1);
                viewport.set_destination(width as i32, height as i32);
            }
            None => surface.set_buffer_scale((island.scale / SCALE_DENOM) as i32),
        }
        surface.damage_buffer(0, 0, bw as i32, bh as i32);
        if island.stage.animating() {
            surface.frame(&self.qh, FrameCallbackData(surface.clone()));
            island.frame_pending = true;
        }
        if let Err(e) = buffer.attach_to(surface) {
            log::error!("attaching buffer: {e}");
            return;
        }
        island.layer.commit();
        if let Some(started) = self.started.take() {
            log::info!("first frame after {} ms", started.elapsed().as_millis());
        }
    }

    fn set_scale(&mut self, surface: &wl_surface::WlSurface, scale: u32) {
        let Some(index) = self.island_mut(surface) else {
            return;
        };
        if self.islands[index].scale != scale {
            self.islands[index].scale = scale;
            self.draw(index);
        }
    }
}

impl CompositorHandler for Shell {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        // Fractional scale events take precedence when available.
        if self.fractional.is_none() {
            self.set_scale(surface, new_factor.max(1) as u32 * SCALE_DENOM);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        time: u32,
    ) {
        if let Some(index) = self.island_mut(surface) {
            self.on_frame(index, time);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Shell {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.add_output(qh, output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.islands.retain(|i| i.output != output);
        self.reattach_keep_awake();
    }
}

impl LayerShellHandler for Shell {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.islands.retain(|i| &i.layer != layer);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(index) = self.island_mut(layer.wl_surface()) else {
            return;
        };
        let (w, h) = configure.new_size;
        let island = &mut self.islands[index];
        let size = (w.max(1), if h == 0 { island.requested_height } else { h });
        island.size = Some(size);
        island.resize_pending = false;
        self.update_stage(index);
    }
}

impl ShmHandler for Shell {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Shell {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl SeatHandler for Shell {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(e) => log::warn!("no keyboard: {e}"),
            }
            return;
        }
        if capability != Capability::Pointer || self.pointer.is_some() {
            return;
        }
        match self.seat_state.get_pointer(qh, &seat) {
            Ok(pointer) => {
                self.cursor_device = self
                    .cursor_shape
                    .as_ref()
                    .map(|m| m.get_shape_device(&pointer, qh));
                self.pointer = Some(pointer);
            }
            Err(e) => log::warn!("no pointer: {e}"),
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
        if capability == Capability::Pointer {
            if let Some(device) = self.cursor_device.take() {
                device.destroy();
            }
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Shell {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.keyboard_focus = self.island_mut(surface);
        log::debug!("keyboard focus: output {:?}", self.keyboard_focus);
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        // Something else took the keyboard (another exclusive layer); close.
        if let Some(index) = self.island_mut(surface) {
            self.unpin(index);
        }
        self.keyboard_focus = None;
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if let Some(index) = self.keyboard_focus {
            self.on_key(index, event.keysym);
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        // Holding left or right keeps moving a selected slider.
        if let Some(index) = self.keyboard_focus
            && let Some(Target::Slider(_)) = self.islands[index].stage.selected_target()
            && matches!(
                event.keysym,
                Keysym::Left | Keysym::Right | Keysym::h | Keysym::l
            )
        {
            self.on_key(index, event.keysym);
        }
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if let Some(index) = self.keyboard_focus
            && matches!(
                event.keysym,
                Keysym::Return | Keysym::KP_Enter | Keysym::space
            )
        {
            self.release(index);
        }
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for Shell {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self.island_mut(&event.surface) else {
                continue;
            };
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(device) = &self.cursor_device {
                        device.set_shape(serial, Shape::Default);
                    }
                    let hover = self.islands[index].stage.hit(x, y);
                    self.set_hover(index, hover);
                    self.set_pointer(index, Some((x, y)));
                }
                PointerEventKind::Motion { .. } => {
                    if let Some((slider, track)) = self.islands[index].drag {
                        self.set_level(slider, dmsh_core::slider_value(track, x));
                    }
                    if self.islands[index].volume_press.is_some()
                        && self.islands[index].stage.target_at(x, y)
                            != Some(Target::Level(Slider::Volume))
                    {
                        self.cancel_volume_press(index);
                    }
                    // Sliding off the held option cancels it.
                    if let Some((action, _)) = self.islands[index].session.hold
                        && self.islands[index].stage.target_at(x, y)
                            != Some(Target::Session(action))
                    {
                        self.cancel_hold(index);
                    }
                    let hover = self.islands[index].stage.hit(x, y);
                    self.set_hover(index, hover);
                    self.set_pointer(index, Some((x, y)));
                }
                PointerEventKind::Leave { .. } => {
                    if self.menu.as_ref().is_some_and(|m| m.output == index) {
                        self.close_menu();
                    }
                    self.cancel_hold(index);
                    self.cancel_volume_press(index);
                    self.islands[index].drag = None;
                    self.set_pointer(index, None);
                    self.set_hover(index, None);
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let island = &mut self.islands[index];
                    let expanded = island.pinned.or(island.hover) == Some(IslandKind::Right);
                    if !expanded || island.stage.hit(x, y) != Some(IslandKind::Right) {
                        continue;
                    }
                    let level = match island.stage.target_at(x, y) {
                        Some(Target::Slider(s) | Target::Level(s)) => Some(s),
                        _ => None,
                    };
                    // Wheels report notches; touchpads report distance.
                    let rows = if vertical.discrete != 0 {
                        vertical.discrete
                    } else {
                        island.scroll_accum += vertical.absolute;
                        let rows = (island.scroll_accum / 24.0).trunc();
                        island.scroll_accum -= rows * 24.0;
                        rows as i32
                    };
                    if rows != 0 {
                        match level {
                            // Scrolling down turns it down.
                            Some(slider) => self.step_level(slider, -(rows as f32) * SLIDER_STEP),
                            None => self.scroll_right(index, rows),
                        }
                    }
                }
                PointerEventKind::Press { button, .. } => {
                    let hit = self.islands[index].stage.hit_rect_at(x, y);
                    if let Some(Hit {
                        target: Target::Slider(slider),
                        rect,
                        ..
                    }) = hit
                        && button == BTN_LEFT
                    {
                        self.islands[index].drag = Some((slider, rect));
                        self.set_level(slider, dmsh_core::slider_value(rect, x));
                    } else if let Some(hit) = hit {
                        self.click(index, hit.target, button);
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    self.release(index);
                }
                _ => {}
            }
        }
    }
}

delegate_registry!(Shell);
smithay_client_toolkit::delegate_dispatch2!(Shell);

// User data for the fractional-scale and viewporter objects, which sctk doesn't wrap.

struct IdleInhibitData;

impl Dispatch2<zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, Shell> for IdleInhibitData {
    fn event(
        &self,
        _: &mut Shell,
        _: &zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
        _: zwp_idle_inhibit_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<Shell>,
    ) {
    }
}

impl Dispatch2<zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1, Shell> for IdleInhibitData {
    fn event(
        &self,
        _: &mut Shell,
        _: &zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        _: zwp_idle_inhibitor_v1::Event,
        _: &Connection,
        _: &QueueHandle<Shell>,
    ) {
    }
}

struct FractionalManagerData;
struct ViewporterData;
struct ViewportData;
/// Identifies which surface a fractional-scale object belongs to.
struct SurfaceData(wl_surface::WlSurface);

impl Dispatch2<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, Shell>
    for FractionalManagerData
{
    fn event(
        &self,
        _: &mut Shell,
        _: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        _: wp_fractional_scale_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<Shell>,
    ) {
    }
}

impl Dispatch2<wp_fractional_scale_v1::WpFractionalScaleV1, Shell> for SurfaceData {
    fn event(
        &self,
        shell: &mut Shell,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<Shell>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            log::debug!("surface {} preferred scale {scale}/120", self.0.id());
            shell.set_scale(&self.0, scale.max(1));
        }
    }
}

impl Dispatch2<wp_viewporter::WpViewporter, Shell> for ViewporterData {
    fn event(
        &self,
        _: &mut Shell,
        _: &wp_viewporter::WpViewporter,
        _: wp_viewporter::Event,
        _: &Connection,
        _: &QueueHandle<Shell>,
    ) {
    }
}

impl Dispatch2<wp_viewport::WpViewport, Shell> for ViewportData {
    fn event(
        &self,
        _: &mut Shell,
        _: &wp_viewport::WpViewport,
        _: wp_viewport::Event,
        _: &Connection,
        _: &QueueHandle<Shell>,
    ) {
    }
}
