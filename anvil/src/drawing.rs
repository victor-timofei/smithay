#![allow(clippy::too_many_arguments, unused_imports)]

use std::{cell::RefCell, sync::Mutex};

#[cfg(feature = "image")]
use image::{ImageBuffer, Rgba};
use slog::Logger;
#[cfg(feature = "image")]
use smithay::backend::renderer::gles2::{Gles2Error, Gles2Renderer, Gles2Texture};
use smithay::{
    backend::{
        renderer::{buffer_type, BufferType, Frame, ImportAll, Renderer, Texture, Transform},
        SwapBuffersError,
    },
    reexports::wayland_server::protocol::{wl_buffer, wl_surface},
    utils::{Logical, Point, Rectangle},
    wayland::{
        compositor::{
            get_role, with_states, with_surface_tree_upward, Damage, SubsurfaceCachedState,
            SurfaceAttributes, TraversalAction,
        },
        seat::CursorImageAttributes,
        shell::wlr_layer::Layer,
    },
};

use crate::shell::SurfaceData;

pub static CLEAR_COLOR: [f32; 4] = [0.8, 0.8, 0.9, 1.0];

/*
pub fn draw_cursor<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    surface: &wl_surface::WlSurface,
    location: Point<i32, Logical>,
    output_scale: f32,
    log: &Logger,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let ret = with_states(surface, |states| {
        Some(
            states
                .data_map
                .get::<Mutex<CursorImageAttributes>>()
                .unwrap()
                .lock()
                .unwrap()
                .hotspot,
        )
    })
    .unwrap_or(None);
    let delta = match ret {
        Some(h) => h,
        None => {
            warn!(
                log,
                "Trying to display as a cursor a surface that does not have the CursorImage role."
            );
            (0, 0).into()
        }
    };
    draw_surface_tree(renderer, frame, surface, location - delta, output_scale, log)
}

pub fn draw_layers<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    window_map: &WindowMap,
    layer: Layer,
    output_rect: Rectangle<i32, Logical>,
    output_scale: f32,
    log: &::slog::Logger,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let mut result = Ok(());

    window_map
        .layers
        .with_layers_from_bottom_to_top(&layer, |layer_surface| {
            // skip layers that do not overlap with a given output
            if !output_rect.overlaps(layer_surface.bbox) {
                return;
            }

            let mut initial_place: Point<i32, Logical> = layer_surface.location;
            initial_place.x -= output_rect.loc.x;

            if let Some(wl_surface) = layer_surface.surface.get_surface() {
                // this surface is a root of a subsurface tree that needs to be drawn
                if let Err(err) =
                    draw_surface_tree(renderer, frame, wl_surface, initial_place, output_scale, log)
                {
                    result = Err(err);
                }

                window_map.with_child_popups(wl_surface, |popup| {
                    let location = popup.location();
                    let draw_location = initial_place + location;
                    if let Some(wl_surface) = popup.get_surface() {
                        if let Err(err) =
                            draw_surface_tree(renderer, frame, wl_surface, draw_location, output_scale, log)
                        {
                            result = Err(err);
                        }
                    }
                });
            }
        });

    result
}

pub fn draw_dnd_icon<R, E, F, T>(
    renderer: &mut R,
    frame: &mut F,
    surface: &wl_surface::WlSurface,
    location: Point<i32, Logical>,
    output_scale: f32,
    log: &::slog::Logger,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    if get_role(surface) != Some("dnd_icon") {
        warn!(
            log,
            "Trying to display as a dnd icon a surface that does not have the DndIcon role."
        );
    }
    draw_surface_tree(renderer, frame, surface, location, output_scale, log)
}

*/

#[cfg(feature = "debug")]
pub static FPS_NUMBERS_PNG: &[u8] = include_bytes!("../resources/numbers.png");

#[cfg(feature = "debug")]
pub fn draw_fps<R, E, F, T>(
    _renderer: &mut R,
    frame: &mut F,
    texture: &T,
    output_scale: f64,
    value: u32,
) -> Result<(), SwapBuffersError>
where
    R: Renderer<Error = E, TextureId = T, Frame = F> + ImportAll,
    F: Frame<Error = E, TextureId = T>,
    E: std::error::Error + Into<SwapBuffersError>,
    T: Texture + 'static,
{
    let value_str = value.to_string();
    let mut offset_x = 0f64;
    for digit in value_str.chars().map(|d| d.to_digit(10).unwrap()) {
        frame
            .render_texture_from_to(
                texture,
                match digit {
                    9 => Rectangle::from_loc_and_size((0, 0), (22, 35)),
                    6 => Rectangle::from_loc_and_size((22, 0), (22, 35)),
                    3 => Rectangle::from_loc_and_size((44, 0), (22, 35)),
                    1 => Rectangle::from_loc_and_size((66, 0), (22, 35)),
                    8 => Rectangle::from_loc_and_size((0, 35), (22, 35)),
                    0 => Rectangle::from_loc_and_size((22, 35), (22, 35)),
                    2 => Rectangle::from_loc_and_size((44, 35), (22, 35)),
                    7 => Rectangle::from_loc_and_size((0, 70), (22, 35)),
                    4 => Rectangle::from_loc_and_size((22, 70), (22, 35)),
                    5 => Rectangle::from_loc_and_size((44, 70), (22, 35)),
                    _ => unreachable!(),
                },
                Rectangle::from_loc_and_size((offset_x, 0.0), (22.0 * output_scale, 35.0 * output_scale)),
                Transform::Normal,
                1.0,
            )
            .map_err(Into::into)?;
        offset_x += 24.0 * output_scale;
    }

    Ok(())
}

#[cfg(feature = "image")]
pub fn import_bitmap<C: std::ops::Deref<Target = [u8]>>(
    renderer: &mut Gles2Renderer,
    image: &ImageBuffer<Rgba<u8>, C>,
) -> Result<Gles2Texture, Gles2Error> {
    use smithay::backend::renderer::gles2::ffi;

    renderer.with_context(|renderer, gl| unsafe {
        let mut tex = 0;
        gl.GenTextures(1, &mut tex);
        gl.BindTexture(ffi::TEXTURE_2D, tex);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE as i32);
        gl.TexImage2D(
            ffi::TEXTURE_2D,
            0,
            ffi::RGBA as i32,
            image.width() as i32,
            image.height() as i32,
            0,
            ffi::RGBA,
            ffi::UNSIGNED_BYTE as u32,
            image.as_ptr() as *const _,
        );
        gl.BindTexture(ffi::TEXTURE_2D, 0);

        Gles2Texture::from_raw(
            renderer,
            tex,
            (image.width() as i32, image.height() as i32).into(),
        )
    })
}
