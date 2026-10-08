//! The GPU side of the overlay, shared by the window hosts: one device, an egui renderer, and
//! drawing frames to a transparent surface.

use anyhow::{Context, Result};
use egui_wgpu::wgpu;
use std::time::{Duration, Instant};

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Created with the first surface, once the output format is known.
    renderer: Option<(egui_wgpu::Renderer, wgpu::TextureFormat)>,
    /// Set when the device is lost (a driver reset or update); nothing can draw after that.
    lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// What happened to a frame.
pub enum Frame {
    Presented,
    /// Nothing drawn this time (the compositor isn't ready); try again later.
    Skipped,
    /// The surface no longer matches the window; configure it and redraw.
    Outdated,
}

impl Gpu {
    pub fn new(instance: wgpu::Instance) -> Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no usable GPU found")?;
        log::info!("rendering with {}", adapter.get_info().name);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("dayz-map"),
                // The default 8192 px is less than a scaled 8K-wide monitor needs.
                required_limits: wgpu::Limits {
                    max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d,
                    ..wgpu::Limits::default()
                },
                ..Default::default()
            }))?;
        let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = lost.clone();
        device.set_device_lost_callback(move |reason, message| {
            log::error!("the graphics device was lost ({reason:?}): {message}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        // wgpu panics on errors nobody captured; log them instead (the first few: one broken
        // resource fails every frame). Running out of graphics memory while the game fills it
        // leaves resources unusable, so start over as if the device were lost.
        let errors = std::sync::atomic::AtomicU32::new(0);
        let flag = lost.clone();
        device.on_uncaptured_error(std::sync::Arc::new(move |error| {
            if errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 10 {
                log::error!("graphics error: {error}");
            }
            if matches!(error, wgpu::Error::OutOfMemory { .. }) {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }));
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            renderer: None,
            lost,
        })
    }

    /// Whether the device is gone, so the overlay has to start over to draw again.
    pub fn is_lost(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Sizes the surface (in pixels) for see-through drawing.
    pub fn configure(&mut self, surface: &wgpu::Surface, width: u32, height: u32) -> Result<()> {
        let caps = surface.get_capabilities(&self.adapter);
        if caps.formats.is_empty() || caps.alpha_modes.is_empty() {
            anyhow::bail!(
                "{} can't draw to this display; with several GPUs, try running on the one \
                 the monitor is connected to",
                self.adapter.get_info().name
            );
        }
        let format = match &self.renderer {
            Some((_, format)) => *format,
            None => {
                let format = egui_wgpu::preferred_framebuffer_format(&caps.formats)
                    .unwrap_or(caps.formats[0]);
                let renderer = egui_wgpu::Renderer::new(
                    &self.device,
                    format,
                    egui_wgpu::RendererOptions::default(),
                );
                self.renderer = Some((renderer, format));
                format
            }
        };
        let alpha_mode = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            log::warn!(
                "the GPU surface can't be transparent here ({:?})",
                caps.alpha_modes
            );
            caps.alpha_modes[0]
        };
        // Windows has no frame callbacks to pace redraws, so wait for the display there.
        let present_mode = if cfg!(windows) {
            wgpu::PresentMode::Fifo
        } else if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        // A surface that can't be set up (no desktop to show on, say) is an error to report,
        // not wgpu's default of panicking.
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                color_space: Default::default(),
                width: width.max(1),
                height: height.max(1),
                present_mode,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats: vec![],
            },
        );
        if let Some(e) = pollster::block_on(scope.pop()) {
            anyhow::bail!("setting up the overlay's drawing surface: {e}");
        }
        Ok(())
    }

    /// Draws egui's output, faded by `opacity` (0 to 1). `before_present` runs just before the
    /// frame is handed to the compositor (Wayland asks for its next frame callback there).
    pub fn paint(
        &mut self,
        surface: &wgpu::Surface,
        ctx: &egui::Context,
        output: egui::FullOutput,
        opacity: f32,
        before_present: impl FnOnce(),
    ) -> Frame {
        let Some((renderer, _)) = self.renderer.as_mut() else {
            return Frame::Outdated;
        };
        let (device, queue) = (&self.device, &self.queue);
        let mut primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        if opacity < 1.0 {
            // Colours are premultiplied, so scaling every channel fades the whole frame.
            for primitive in &mut primitives {
                if let egui::epaint::Primitive::Mesh(mesh) = &mut primitive.primitive {
                    for vertex in &mut mesh.vertices {
                        vertex.color = vertex.color.linear_multiply(opacity);
                    }
                }
            }
        }
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        // egui reports each freed texture only once, so free them even if nothing is drawn.
        let free = |renderer: &mut egui_wgpu::Renderer| {
            for id in &output.textures_delta.free {
                renderer.free_texture(id);
            }
        };
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                free(renderer);
                return Frame::Skipped;
            }
            _ => {
                free(renderer);
                return Frame::Outdated;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [frame.texture.width(), frame.texture.height()],
            pixels_per_point: output.pixels_per_point,
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let mut commands =
            renderer.update_buffers(device, queue, &mut encoder, &primitives, &screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("overlay"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            renderer.render(&mut pass, &primitives, &screen);
        }
        commands.push(encoder.finish());
        queue.submit(commands);
        before_present();
        queue.present(frame);
        free(renderer);
        Frame::Presented
    }

    /// Runs an empty egui pass so textures the app dropped are freed on the GPU now, rather
    /// than the next time the overlay opens.
    pub fn release_textures(&mut self, ctx: &egui::Context) {
        let Some((renderer, _)) = self.renderer.as_mut() else {
            return;
        };
        let output = ctx.run_ui(egui::RawInput::default(), |_| {});
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        for id in &output.textures_delta.free {
            renderer.free_texture(id);
        }
    }
}

/// How the overlay fades in: `Some(duration)` when the host draws the fade itself.
pub struct Fade {
    duration: Option<Duration>,
    shown_at: Instant,
}

impl Fade {
    pub fn new(duration: Option<Duration>) -> Self {
        Self {
            duration,
            shown_at: Instant::now(),
        }
    }

    pub fn restart(&mut self) {
        self.shown_at = Instant::now();
    }

    /// Opacity for the next frame, and whether the fade is still running.
    pub fn opacity(&self) -> (f32, bool) {
        match self.duration {
            Some(d) => {
                let t = self.shown_at.elapsed().as_secs_f32() / d.as_secs_f32();
                (t.min(1.0), t < 1.0)
            }
            None => (1.0, false),
        }
    }
}

/// When egui wants its next frame: `None` for never, `Some(None)` for now.
pub fn repaint_after(output: &egui::FullOutput) -> Option<Option<Instant>> {
    repaint_in(
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)?
            .repaint_delay,
    )
}

/// The same for a delay egui asked for (it uses a huge delay to mean "never").
pub fn repaint_in(delay: Duration) -> Option<Option<Instant>> {
    if delay.is_zero() {
        Some(None)
    } else if delay < Duration::from_secs(3600) {
        Some(Some(Instant::now() + delay))
    } else {
        None
    }
}
