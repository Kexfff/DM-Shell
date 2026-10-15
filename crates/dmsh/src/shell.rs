//! Wayland side: one full-width layer surface per output, both islands drawn into it.

use anyhow::{Context, Result};
use dmsh_core::{Layout, Theme};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::EventLoop,
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

    let mut shell = Shell {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        pool: SlotPool::new(1 << 20, &shm)?,
        shm,
        fractional,
        theme: Theme::default(),
        islands: Vec::new(),
    };

    let mut event_loop: EventLoop<Shell> = EventLoop::try_new()?;
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("inserting Wayland source: {e}"))?;
    event_loop.run(None, &mut shell, |_| {})?;
    Ok(())
}

struct Shell {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    fractional: Option<(
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_viewporter::WpViewporter,
    )>,
    theme: Theme,
    islands: Vec<OutputSurface>,
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
    /// Output scale in 1/120ths.
    scale: u32,
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
        layer.set_size(0, self.theme.surface_height);
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
        self.islands.push(OutputSurface {
            output,
            layer,
            scaling,
            size: None,
            scale,
        });
    }

    fn island_mut(&mut self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.islands
            .iter()
            .position(|i| i.layer.wl_surface() == surface)
    }

    fn draw(&mut self, index: usize) {
        let island = &self.islands[index];
        let Some((width, height)) = island.size else {
            return;
        };
        // Rounding rule from the fractional-scale protocol: half away from zero.
        let to_buffer = |v: u32| ((v * island.scale + SCALE_DENOM / 2) / SCALE_DENOM).max(1);
        let (bw, bh) = (to_buffer(width), to_buffer(height));

        let layout = Layout::compute(width as f32, &self.theme);
        let scene = layout.scene(&self.theme);

        let (buffer, canvas) = match self.pool.create_buffer(
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
            for rect in scene.input_rects() {
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
        let size = (w.max(1), if h == 0 { self.theme.surface_height } else { h });
        self.islands[index].size = Some(size);
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
