//! What a new window shows before the GPU is ready: the desktop theme's
//! window colour, with the view's colour where the last window had its
//! view, drawn by the CPU (softbuffer).  Starting the GPU takes 150 to
//! 800 ms, and a Wayland window appears only once it has a picture, so
//! without this the window would appear with its first GPU frame.  X11 and
//! Windows would show whatever was beneath the new window until then.
//! macOS fills a new window with its own background colour, so it has no
//! placeholder.
//!
//! The view's place comes from the last window (`Saved::view_insets`, the
//! view's distances from the window's edges in points), since panels keep
//! their widths and bars their heights while the view takes the rest.

use std::sync::Arc;
use winit::window::Window;

/// Insets of the view from the window's left, top, right and bottom edges,
/// in points.
pub type ViewInsets = [f32; 4];

#[cfg(any(target_os = "linux", windows))]
pub struct Placeholder {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    window_color: u32,
    view_color: u32,
    view: Option<ViewInsets>,
}

#[cfg(any(target_os = "linux", windows))]
impl Placeholder {
    /// A placeholder for `window` in `theme`'s colours, or `None` where
    /// the window system won't take a CPU picture.
    pub fn new(window: &Arc<Window>, theme: &slerp_theme::DesktopTheme, view: Option<ViewInsets>) -> Option<Self> {
        let context = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&context, window.clone()).ok()?;
        let visuals = slerp_theme::style_for(theme).visuals;
        Some(Placeholder {
            surface,
            window_color: rgb(visuals.panel_fill),
            view_color: rgb(visuals.extreme_bg_color),
            view,
        })
    }

    /// Draws the placeholder at the window's size.
    pub fn paint(&mut self, window: &Window) {
        let size = window.inner_size();
        let (Some(w), Some(h)) = (std::num::NonZeroU32::new(size.width), std::num::NonZeroU32::new(size.height)) else {
            return;
        };
        if self.surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = self.surface.buffer_mut() else { return };
        buffer.fill(self.window_color);
        let (w, h) = (size.width as usize, size.height as usize);
        if let Some([left, top, right, bottom]) = self.view {
            let s = window.scale_factor() as f32;
            let px = |points: f32, limit: usize| ((points * s).round().max(0.0) as usize).min(limit);
            let (x0, y0) = (px(left, w), px(top, h));
            let (x1, y1) = (w.saturating_sub(px(right, w)), h.saturating_sub(px(bottom, h)));
            if x0 < x1 && y0 < y1 {
                for row in buffer.chunks_exact_mut(w).take(y1).skip(y0) {
                    row[x0..x1].fill(self.view_color);
                }
            }
        }
        let _ = buffer.present();
    }
}

/// macOS needs none: its new windows show their background colour.
#[cfg(not(any(target_os = "linux", windows)))]
pub struct Placeholder;

#[cfg(not(any(target_os = "linux", windows)))]
impl Placeholder {
    pub fn new(_: &Arc<Window>, _: &slerp_theme::DesktopTheme, _: Option<ViewInsets>) -> Option<Self> {
        None
    }

    pub fn paint(&mut self, _: &Window) {}
}

/// An opaque egui colour as softbuffer's `0RGB` pixel.
#[cfg(any(target_os = "linux", windows))]
fn rgb(c: egui::Color32) -> u32 {
    (u32::from(c.r()) << 16) | (u32::from(c.g()) << 8) | u32::from(c.b())
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    #[test]
    fn colours_become_0rgb_pixels() {
        assert_eq!(super::rgb(egui::Color32::from_rgb(0x12, 0x34, 0x56)), 0x0012_3456);
    }
}
