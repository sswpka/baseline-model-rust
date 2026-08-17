//! Right-click "image" menu for `egui_plot::Plot`s, shared by every mode
//! (Baseline/Calibration via `channel.rs`, Flux, and Observation): Save
//! image as..., Copy image, Zoom to fit data, Open image in new window.
//! This is the deferred `CalibrationDetailWindow`/`OpenZoomWindow` feature
//! from the original app, generalized to every plot via one mechanism
//! instead of a per-mode detail window.
//!
//! `egui`'s screenshot API (`ViewportCommand::Screenshot` /
//! `Event::Screenshot`) captures the whole native window, not a widget, and
//! carries no per-request tag - so only one export request is tracked at a
//! time (`pending`), and the plot's on-screen rect is cropped out of the
//! next screenshot that arrives.

use std::borrow::Cow;
use std::collections::HashSet;

enum ExportAction {
    Save,
    Copy,
    OpenWindow,
}

struct PendingExport {
    rect: egui::Rect,
    action: ExportAction,
    title: String,
}

struct OpenImageWindow {
    id: egui::ViewportId,
    title: String,
    texture: egui::TextureHandle,
    /// Display size in points (the texture itself is sized in physical
    /// pixels, since it comes from a screenshot captured at
    /// `pixels_per_point` scale).
    size: egui::Vec2,
}

#[derive(Default)]
pub struct PlotExportQueue {
    pending: Option<PendingExport>,
    /// `id_source` strings (matching each plot's `Plot::new(id_source)`)
    /// that should have `.reset()` applied on their next `show()`.
    zoom_to_fit: HashSet<String>,
    open_windows: Vec<OpenImageWindow>,
    next_window_id: u64,
}

impl PlotExportQueue {
    /// Call once per frame, before any mode UI runs: consumes this frame's
    /// `Event::Screenshot` (if any) against `pending` and dispatches it.
    pub fn process_screenshot(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending.take() else {
            return;
        };

        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });

        let Some(image) = image else {
            // Not delivered yet - keep waiting next frame.
            self.pending = Some(pending);
            ctx.request_repaint();
            return;
        };

        let ppp = ctx.pixels_per_point();
        let cropped = crop(&image, pending.rect, ppp);
        if cropped.size[0] == 0 || cropped.size[1] == 0 {
            return;
        }

        match pending.action {
            ExportAction::Save => save_image(&cropped, &pending.title),
            ExportAction::Copy => copy_image(&cropped),
            ExportAction::OpenWindow => self.open_window(ctx, &cropped, &pending.title, ppp),
        }
    }

    /// Call once per frame, unconditionally (independent of which mode tab
    /// is active), so windows opened from any mode keep rendering.
    pub fn show_open_windows(&mut self, ctx: &egui::Context) {
        self.open_windows.retain(|w| {
            ctx.show_viewport_immediate(
                w.id,
                egui::ViewportBuilder::default()
                    .with_title(&w.title)
                    .with_inner_size(w.size),
                |ctx, _class| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.add(
                            egui::Image::new((w.texture.id(), w.texture.size_vec2()))
                                .fit_to_exact_size(w.size),
                        );
                    });
                    !ctx.input(|i| i.viewport().close_requested())
                },
            )
        });
    }

    fn open_window(
        &mut self,
        ctx: &egui::Context,
        image: &egui::ColorImage,
        title: &str,
        ppp: f32,
    ) {
        self.next_window_id += 1;
        let id = egui::ViewportId::from_hash_of(("plot_export_window", self.next_window_id));
        let texture = ctx.load_texture(
            format!("plot_export_{}", self.next_window_id),
            image.clone(),
            egui::TextureOptions::default(),
        );
        let size = egui::vec2(image.size[0] as f32, image.size[1] as f32) / ppp;
        self.open_windows.push(OpenImageWindow {
            id,
            title: title.to_string(),
            texture,
            size,
        });
    }
}

/// Wraps `plot.show(...)` with the right-click image menu. `id_source` must
/// be the same string passed to `Plot::new(id_source)`, so "Zoom to fit
/// data" can flag the right plot for `.reset()` on its next frame. `title`
/// seeds the save-dialog default filename and the new window's title.
pub fn show<R>(
    ui: &mut egui::Ui,
    export: &mut PlotExportQueue,
    id_source: &str,
    title: &str,
    plot: egui_plot::Plot<'_>,
    build_fn: impl FnOnce(&mut egui_plot::PlotUi) -> R,
) -> R {
    let plot = if export.zoom_to_fit.remove(id_source) {
        plot.reset()
    } else {
        plot
    };

    // Mutated in place (not via `ui.scope`, which would parent the plot under
    // a fresh auto-id `Ui` and change what `ui.make_persistent_id(id_source)`
    // resolves to inside `Plot::show`, orphaning any saved pan/zoom state) and
    // restored right after, since callers keep drawing on this same `ui`
    // (e.g. the stats label below `histogram_plot_sized`'s plot) and
    // shouldn't inherit the plot-only background/text color.
    //
    // `override_text_color` keeps egui_plot's axis ticks and grid readable
    // against the plot-only black background.
    let old_bg = ui.visuals().extreme_bg_color;
    let old_text = ui.visuals().override_text_color;
    ui.visuals_mut().extreme_bg_color = egui::Color32::BLACK;
    ui.visuals_mut().override_text_color = Some(egui::Color32::WHITE);
    let response = plot.show(ui, build_fn);
    ui.visuals_mut().extreme_bg_color = old_bg;
    ui.visuals_mut().override_text_color = old_text;

    let rect = response.response.rect;

    response.response.context_menu(|ui| {
        if ui.button("Save image as...").clicked() {
            export.pending = Some(PendingExport {
                rect,
                action: ExportAction::Save,
                title: title.to_string(),
            });
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot);
            ui.close_menu();
        }
        if ui.button("Copy image").clicked() {
            export.pending = Some(PendingExport {
                rect,
                action: ExportAction::Copy,
                title: title.to_string(),
            });
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot);
            ui.close_menu();
        }
        if ui.button("Zoom to fit data").clicked() {
            export.zoom_to_fit.insert(id_source.to_string());
            ui.close_menu();
        }
        if ui.button("Open image in new window").clicked() {
            export.pending = Some(PendingExport {
                rect,
                action: ExportAction::OpenWindow,
                title: title.to_string(),
            });
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot);
            ui.close_menu();
        }
    });

    response.inner
}

/// Crops a full-window screenshot (in physical pixels) down to `rect`
/// (in points), converting via `pixels_per_point`.
fn crop(image: &egui::ColorImage, rect: egui::Rect, ppp: f32) -> egui::ColorImage {
    let [img_w, img_h] = image.size;
    let x0 = ((rect.min.x * ppp).round() as i64).clamp(0, img_w as i64) as usize;
    let y0 = ((rect.min.y * ppp).round() as i64).clamp(0, img_h as i64) as usize;
    let x1 = ((rect.max.x * ppp).round() as i64).clamp(x0 as i64, img_w as i64) as usize;
    let y1 = ((rect.max.y * ppp).round() as i64).clamp(y0 as i64, img_h as i64) as usize;
    let w = x1 - x0;
    let h = y1 - y0;

    let mut pixels = Vec::with_capacity(w * h);
    for y in y0..y1 {
        let row_start = y * img_w + x0;
        pixels.extend_from_slice(&image.pixels[row_start..row_start + w]);
    }
    egui::ColorImage {
        size: [w, h],
        pixels,
    }
}

fn to_rgba_bytes(image: &egui::ColorImage) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }
    bytes
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect()
}

fn save_image(image: &egui::ColorImage, title: &str) {
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(format!("{}.png", sanitize_filename(title)))
        .add_filter("PNG Image", &["png"])
        .save_file()
    else {
        return;
    };

    let [w, h] = image.size;
    let bytes = to_rgba_bytes(image);
    let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) else {
        eprintln!("Failed to build image buffer for plot screenshot");
        return;
    };
    if let Err(e) = buf.save(&path) {
        eprintln!("Failed to save plot image to {}: {e}", path.display());
    }
}

fn copy_image(image: &egui::ColorImage) {
    let [w, h] = image.size;
    let bytes = to_rgba_bytes(image);
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            let data = arboard::ImageData {
                width: w,
                height: h,
                bytes: Cow::Owned(bytes),
            };
            if let Err(e) = clipboard.set_image(data) {
                eprintln!("Failed to copy plot image to clipboard: {e}");
            }
        }
        Err(e) => eprintln!("Failed to access clipboard: {e}"),
    }
}
