//! Wayland side: one full-width layer surface per output, both islands drawn into it.

use std::time::Instant;

use anyhow::{Context, Result};
use dmsh_core::{
    Activities, Layout, Theme, View,
    ipc::{Reply, Request, Response, StateDump},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_output, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
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

    let mut shell = Shell {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        fractional,
        theme: Theme::default(),
        islands: Vec::new(),
        activities: Activities::default(),
        view: View::default(),
        loop_handle: event_loop.handle(),
        timer: None,
    };

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("inserting Wayland source: {e}"))?;
    event_loop.run(None, &mut shell, |_| {})?;
    Ok(())
}

pub struct Shell {
    registry_state: RegistryState,
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
    view: View,
    loop_handle: LoopHandle<'static, Shell>,
    /// Fires at the arbiter's next deadline (expiry or center→right move).
    timer: Option<RegistrationToken>,
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
            Request::Dismiss { id } => Ok(Reply::Dismissed {
                existed: self.activities.dismiss(&id),
            }),
            Request::Clear => {
                self.activities.clear();
                Ok(Reply::Done)
            }
            Request::State => Ok(Reply::State(StateDump {
                view: self.activities.view(now),
                activities: self.activities.states(now),
            })),
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
        self.activities.expire(now);
        let view = self.activities.view(now);
        if view != self.view {
            self.view = view;
            for i in 0..self.islands.len() {
                self.draw(i);
            }
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

        let layout = Layout::compute(width as f32, &self.theme, &self.view);
        let scene = layout.scene(&self.theme, &self.view);

        // The canvas is only as tall as its content: the strip while collapsed, taller
        // while something floats over windows. The reserved strip never changes.
        let needed = scene.canvas_height(&self.theme);
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
        if !dmsh_render::render_argb8888(&scene, px_scale, canvas, bw, bh) {
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

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

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
        self.draw(index);
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
    registry_handlers![OutputState];
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
