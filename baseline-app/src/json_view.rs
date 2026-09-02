//! Helpers for the "JSON" tab in each mode: it renders a preview of the
//! decoded Data Table rows as JSON and offers a "Download JSON..." button that writes every row to a file.

use serde_json::Value;
use std::path::PathBuf;

pub const PREVIEW_ROWS: usize = 20;

/// Parses `s` into a JSON number when it looks like one, otherwise keeps it as
/// a string (hex markers like `E225`, timestamps, reserved/checksum spans).
pub fn num_or_str(s: &str) -> Value {
    let t = s.trim();
    if let Ok(i) = t.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = t.parse::<f64>() {
        if f.is_finite() {
            return Value::from(f);
        }
    }
    Value::from(t)
}

/// Builds an ordered JSON object from `(key, value)` pairs, keeping insertion
/// order (serde_json's `preserve_order` feature is enabled).
pub fn object<I>(pairs: I) -> Value
where
    I: IntoIterator<Item = (String, Value)>,
{
    Value::Object(pairs.into_iter().collect())
}

/// Pretty-prints the first [`PREVIEW_ROWS`] of `values` for the on-screen
/// preview. Returns an empty string when there is nothing to show.
pub fn preview_string(values: &[Value]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let shown = &values[..values.len().min(PREVIEW_ROWS)];
    serde_json::to_string_pretty(shown).unwrap_or_default()
}

/// Renders the JSON tab body. Returns `true` when the download button was
/// clicked this frame.
pub fn json_tab_ui(ui: &mut egui::Ui, row_count: usize, preview_json: &str) -> bool {
    let mut download_clicked = false;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(row_count > 0, egui::Button::new("Download JSON..."))
            .clicked()
        {
            download_clicked = true;
        }
        ui.label(format!("{row_count} row(s)"));
        if row_count > PREVIEW_ROWS {
            ui.separator();
            ui.label(format!(
                "Preview shows the first {PREVIEW_ROWS}"
            ));
        }
    });
    ui.separator();

    if preview_json.is_empty() {
        ui.label("No data - run Process Data first.");
        return download_clicked;
    }

    egui::ScrollArea::both()
        .id_salt("json_preview_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(preview_json).monospace())
                    .selectable(true)
                    .extend(),
            );
        });
    download_clicked
}

/// Prompts for a path and writes `values` as pretty JSON. `Ok(None)` means the
/// user cancelled the dialog.
pub fn save_json(default_name: &str, values: &[Value]) -> Result<Option<PathBuf>, String> {
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter("JSON", &["json"])
        .save_file()
    else {
        return Ok(None);
    };
    let text = serde_json::to_string_pretty(values).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_or_str_parses_numbers_but_keeps_markers() {
        assert_eq!(num_or_str(" 42 "), Value::from(42));
        assert_eq!(num_or_str("3.5"), Value::from(3.5));
        assert_eq!(num_or_str("E225"), Value::from("E225"));
    }

    #[test]
    fn object_preserves_insertion_order() {
        let v = object([
            ("b".to_string(), 1.into()),
            ("a".to_string(), 2.into()),
        ]);
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"b":1,"a":2}"#);
    }
}
