//! Wayland side: one full-width layer surface per output, both islands drawn into it.

use std::time::Instant;

use anyhow::{Context, Result};
use dmsh_core::{
    Activities, IslandKind, Stage, Theme, View,
    ipc::{Reply, Request, Response, StateDump},
};
use dmsh_render::Renderer;
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
            protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
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
    viewporter::client::{wp_viewport, wp_viewporter},
};

/// Fractional scales arrive as multiples of 1/120.
const SCALE_DENOM: u32 = 120;

pub fn run() -> Result<()> {
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
    let mut shell = Shell {
        seat_state: SeatState::new(&globals, &qh),
        pointer: None,
        cursor_shape,
        cursor_device: None,
        renderer: Renderer::new(),
        qh: qh.clone(),
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        fractional,
        theme: Theme::default(),
        islands: Vec::new(),
        activities: Activities::default(),
        views: Views::default(),
        focused_output: None,
        clock: None,
        date: None,
        loop_handle: event_loop.handle(),
        timer: None,
        sources: None,
    };

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
    match dmsh_sources::spawn(move |event| {
        let _ = source_tx.send(event);
    }) {
        Ok(handle) => shell.sources = Some(handle),
        Err(e) => log::error!("starting sources: {e}"),
    }

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("inserting Wayland source: {e}"))?;
    event_loop.run(None, &mut shell, |_| {})?;
    Ok(())
}

pub struct Shell {
    registry_state: RegistryState,
    seat_state: SeatState,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_shape: Option<CursorShapeManager>,
    cursor_device: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    /// Fonts and glyph caches, shared by every output; also measures text for layout.
    renderer: Renderer,
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
    /// A frame callback is outstanding; the next draw happens when it fires.
    frame_pending: bool,
    /// Timestamp of the last animation frame, for frame-rate independent springs.
    last_frame_ms: Option<u32>,
}

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
            Request::Push(spec) => self.activities.push(spec, now).map(|()| Reply::Done),
            Request::Dismiss { id } => {
                let existed = self.activities.dismiss(&id);
                if existed {
                    self.notify_removed(id, Removal::Dismissed);
                }
                Ok(Reply::Dismissed { existed })
            }
            Request::Clear => {
                for id in self.activities.clear() {
                    self.notify_removed(id, Removal::Dismissed);
                }
                Ok(Reply::Done)
            }
            Request::State => Ok(Reply::State(Box::new(StateDump {
                view: self.views.focused.clone(),
                focused_output: self.focused_output.clone(),
                activities: self.activities.states(now),
            }))),
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
            SourceEvent::Clock { time, date } => {
                self.clock = Some(time);
                self.date = Some(date);
                self.refresh();
            }
        }
    }

    /// Lets the owning source know (e.g. to emit `NotificationClosed`).
    fn notify_removed(&self, id: String, reason: Removal) {
        if let Some(sources) = &self.sources {
            sources.send(SourceCommand::Removed { id, reason });
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
        let view = if focused {
            &self.views.focused
        } else {
            &self.views.other
        };
        let island = &mut self.islands[index];
        let Some((width, _)) = island.size else {
            return;
        };
        island.stage.update(
            width as f32,
            view,
            island.hover,
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
            let hover = match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(device) = &self.cursor_device {
                        device.set_shape(serial, Shape::Default);
                    }
                    self.islands[index].stage.hit(x, y)
                }
                PointerEventKind::Motion { .. } => self.islands[index].stage.hit(x, y),
                PointerEventKind::Leave { .. } => None,
                _ => continue,
            };
            self.set_hover(index, hover);
        }
    }
}

delegate_registry!(Shell);
smithay_client_toolkit::delegate_dispatch2!(Shell);

// User data for the fractional-scale and viewporter objects, which sctk doesn't wrap.

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
