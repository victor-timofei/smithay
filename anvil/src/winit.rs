use std::{cell::RefCell, rc::Rc, sync::atomic::Ordering, time::Duration};

#[cfg(feature = "debug")]
use smithay::backend::renderer::gles2::Gles2Texture;
#[cfg(feature = "egl")]
use smithay::{
    backend::renderer::{ImportDma, ImportEgl},
    wayland::dmabuf::init_dmabuf_global,
};
use smithay::{
    backend::{
        winit::{self, WinitEvent},
        SwapBuffersError,
    },
    desktop::RenderError,
    reexports::{
        calloop::EventLoop,
        wayland_server::{protocol::wl_output, Display},
    },
    wayland::{
        output::{Mode, Output, PhysicalProperties},
        seat::CursorImageStatus,
    },
};

use slog::Logger;

use crate::drawing::*;
use crate::state::{AnvilState, Backend};

pub const OUTPUT_NAME: &str = "winit";

pub struct WinitData {
    #[cfg(feature = "debug")]
    fps_texture: Gles2Texture,
    #[cfg(feature = "debug")]
    pub fps: fps_ticker::Fps,
}

impl Backend for WinitData {
    fn seat_name(&self) -> String {
        String::from("winit")
    }
}

pub fn run_winit(log: Logger) {
    let mut event_loop = EventLoop::try_new().unwrap();
    let display = Rc::new(RefCell::new(Display::new()));

    let (renderer, mut winit) = match winit::init(log.clone()) {
        Ok(ret) => ret,
        Err(err) => {
            slog::crit!(log, "Failed to initialize Winit backend: {}", err);
            return;
        }
    };
    let renderer = Rc::new(RefCell::new(renderer));

    #[cfg(feature = "egl")]
    if renderer
        .borrow_mut()
        .renderer()
        .bind_wl_display(&display.borrow())
        .is_ok()
    {
        info!(log, "EGL hardware-acceleration enabled");
        let dmabuf_formats = renderer
            .borrow_mut()
            .renderer()
            .dmabuf_formats()
            .cloned()
            .collect::<Vec<_>>();
        let renderer = renderer.clone();
        init_dmabuf_global(
            &mut *display.borrow_mut(),
            dmabuf_formats,
            move |buffer, _| renderer.borrow_mut().renderer().import_dmabuf(buffer).is_ok(),
            log.clone(),
        );
    };

    let size = renderer.borrow().window_size().physical_size;

    /*
     * Initialize the globals
     */

    let data = WinitData {
        #[cfg(feature = "debug")]
        fps_texture: import_bitmap(
            renderer.borrow_mut().renderer(),
            &image::io::Reader::with_format(std::io::Cursor::new(FPS_NUMBERS_PNG), image::ImageFormat::Png)
                .decode()
                .unwrap()
                .to_rgba8(),
        )
        .expect("Unable to upload FPS texture"),
        #[cfg(feature = "debug")]
        fps: fps_ticker::Fps::default(),
    };
    let mut state = AnvilState::init(display.clone(), event_loop.handle(), data, log.clone(), true);

    let mode = Mode {
        size,
        refresh: 60_000,
    };

    let (output, _global) = Output::new(
        &mut *display.borrow_mut(),
        OUTPUT_NAME.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: wl_output::Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
        },
        log.clone(),
    );
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    state.space.borrow_mut().map_output(&output, 1.0, (0, 0).into());

    let start_time = std::time::Instant::now();
    let mut cursor_visible = true;

    #[cfg(feature = "xwayland")]
    state.start_xwayland();

    info!(log, "Initialization completed, starting the main loop.");

    while state.running.load(Ordering::SeqCst) {
        if winit
            .dispatch_new_events(|event| match event {
                WinitEvent::Resized { size, .. } => {
                    let mut space = state.space.borrow_mut();
                    // We only have one output
                    let output = space.outputs().next().unwrap().clone();
                    let current_scale = space.output_scale(&output).unwrap();
                    space.map_output(&output, current_scale, (0, 0).into());
                    output.change_current_state(
                        Some(Mode {
                            size,
                            refresh: 60_000,
                        }),
                        None,
                        None,
                        None,
                    );
                }

                WinitEvent::Input(event) => state.process_input_event_windowed(event, OUTPUT_NAME),

                _ => (),
            })
            .is_err()
        {
            state.running.store(false, Ordering::SeqCst);
            break;
        }

        // drawing logic
        {
            let mut renderer = renderer.borrow_mut();
            // We would need to support EGL_EXT_buffer_age for winit to use age, so lets not bother instead.
            // TODO: Make WinitGraphicsBackend a renderer that delegates to Gles2Renderer and adjusts the transformation instead...
            let result = renderer
                .render(|renderer, _| {
                    state
                        .space
                        .borrow_mut()
                        .render_output(&mut *renderer, &output, 0, CLEAR_COLOR)
                })
                .and_then(|x| {
                    x.map_err(|err| match err {
                        RenderError::OutputNoMode => unreachable!(),
                        RenderError::Rendering(err) => err.into(),
                    })
                });
            if let Err(SwapBuffersError::ContextLost(err)) = result {
                error!(log, "Critical Rendering Error: {}", err);
                state.running.store(false, Ordering::SeqCst);
            }
        }

        // Send frame events so that client start drawing their next frame
        state
            .space
            .borrow()
            .send_frames(false, start_time.elapsed().as_millis() as u32);
        display.borrow_mut().flush_clients(&mut state);

        if event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .is_err()
        {
            state.running.store(false, Ordering::SeqCst);
        } else {
            state.space.borrow_mut().refresh();
            display.borrow_mut().flush_clients(&mut state);
        }

        #[cfg(feature = "debug")]
        state.backend_data.fps.tick();
    }
}
