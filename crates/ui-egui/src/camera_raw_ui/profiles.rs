//! Imported DCPs are user data in the preference store, never bundled Adobe assets.
mod tutorial;

use super::*;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use photocraft_io::raw::CameraProfile;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

// Cancelled dialogs may leave a decoder finishing. Keep only one full RAW
// profile develop live; queued workers check cancellation before allocating.
static PROFILE_DEVELOP: Mutex<()> = Mutex::new(());

const KEY: &str = "cameraRaw.dcpLibrary";
const MAX_FILE: usize = 16 * 1024 * 1024;
const MAX_LIBRARY: usize = 64 * 1024 * 1024;
const MAX_PROFILES: usize = 512;

#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    file: String,
    data: String,
}

struct Entry {
    stored: Stored,
    profile: Arc<CameraProfile>,
    bytes: Arc<Vec<u8>>,
}

#[derive(Default)]
struct Library {
    entries: Vec<Entry>,
    size: usize,
}

impl Library {
    fn import(&mut self, file: &str, bytes: &[u8]) -> Result<usize, String> {
        if bytes.len() > MAX_FILE {
            return Err("DCP file exceeds the 16 MiB limit".into());
        }
        if let Some(i) = self.entries.iter().position(|e| e.bytes.as_slice() == bytes) {
            return Ok(i);
        }
        if self.entries.len() >= MAX_PROFILES || self.size.saturating_add(bytes.len()) > MAX_LIBRARY {
            return Err("DCP library is full (512 profiles or 64 MiB)".into());
        }
        let profile = Arc::new(CameraProfile::parse(bytes).map_err(|e| e.to_string())?);
        let stored = Stored { file: crate::file_open::display_name(file), data: BASE64.encode(bytes) };
        self.size += bytes.len();
        self.entries.push(Entry { stored, profile, bytes: Arc::new(bytes.to_vec()) });
        Ok(self.entries.len() - 1)
    }

    fn load(value: Option<&Value>) -> Self {
        let mut result = Self::default();
        if let Some(entries) = value.and_then(Value::as_array) {
            for item in entries.iter().take(MAX_PROFILES) {
                let Some(data) = item.get("data").and_then(Value::as_str).filter(|s| s.len() <= MAX_FILE.div_ceil(3) * 4) else { continue };
                let Some(file) = item.get("file").and_then(Value::as_str).filter(|s| s.len() <= 4096) else { continue };
                if let Ok(bytes) = BASE64.decode(data) {
                    let _ = result.import(file, &bytes);
                }
            }
        }
        result
    }

    fn save(&self) -> Value {
        json!(self.entries.iter().map(|e| &e.stored).collect::<Vec<_>>())
    }
}

struct Preview {
    surface: photocraft_raster::Surface,
    curve: Vec<[f32; 2]>,
    raw: RawOpen,
}

pub(super) struct Panel {
    library: Library,
    selected: Option<usize>,
    search: String,
    custom_wb: bool,
    wb: [f64; 3],
    name: String,
    calibration: [f64; 3],
    pending: Option<mpsc::Receiver<Result<Preview, String>>>,
    cancelled: Arc<AtomicBool>,
    error: Option<String>,
    tutorial: tutorial::Tutorial,
}

impl Default for Panel {
    fn default() -> Self {
        Self::with_library(Library::default())
    }
}

impl Panel {
    fn with_library(library: Library) -> Self {
        Self {
            library,
            selected: None,
            search: String::new(),
            custom_wb: false,
            wb: [1.0; 3],
            name: "Infrared custom".into(),
            calibration: [1.0; 3],
            pending: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            error: None,
            tutorial: tutorial::Tutorial::default(),
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Panel {
    pub(super) fn load(app: &PhotocraftApp) -> Self {
        Self::with_library(Library::load(app.session.prefs().dialogs.get(KEY)))
    }

    pub(super) fn needs_apply(&self, raw: &RawOpen) -> bool {
        let selected = self.selected.and_then(|i| self.library.entries.get(i)).map(|e| &e.profile);
        selected != raw.camera_profile.as_ref() || self.custom_wb.then_some(self.wb) != raw.white_balance_multipliers
    }

    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn describe(&self, raw: Option<&RawOpen>) -> Value {
        json!({"library": self.library.entries.iter().enumerate().map(|(i,e)| json!({"index":i,"name":e.profile.name,"cameraModel":e.profile.camera_model})).collect::<Vec<_>>(),
            "selected":self.selected,"applied":raw.and_then(|r| r.camera_profile.as_ref().map(|p| p.name.clone())),"previewPending":self.pending.is_some(),"error":self.error,
            "customWhiteBalance":self.custom_wb,"whiteBalanceMultipliers":self.wb,"tutorial":self.tutorial.describe()})
    }
}

pub(super) fn panel(ui: &mut egui::Ui, d: &mut CameraRawDialog) -> Option<&'static str> {
    let p = &mut d.profiles;
    let mut action = None;
    section(ui, "Camera Profile / Infrared", true, |ui| {
        p.tutorial.ui(ui);
        if d.raw_open.is_none() {
            ui.label(tl!("Open a RAW file to use DCP camera profiles."));
            return;
        }
        ui.label(tl!("Search profiles"));
        ui.text_edit_singleline(&mut p.search);
        let selected = p.selected.and_then(|i| p.library.entries.get(i)).map(|e| e.profile.name.as_str()).unwrap_or("Embedded / default");
        egui::ComboBox::from_id_salt("dcp-profile").selected_text(selected).width(270.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut p.selected, None, tl!("Embedded / default"));
            let query = p.search.to_lowercase();
            for (i, e) in p.library.entries.iter().enumerate() {
                let label = match &e.profile.camera_model {
                    Some(m) => format!("{} · {m}", e.profile.name),
                    None => e.profile.name.clone(),
                };
                if query.is_empty() || label.to_lowercase().contains(&query) {
                    ui.selectable_value(&mut p.selected, Some(i), label);
                }
            }
        });
        if ui.button(tl!("Import DCP profiles…")).clicked() {
            action = Some("dcp-import");
        }
        widgets::checkbox(ui, &mut p.custom_wb, tl!("Custom sensor white balance"));
        if p.custom_wb {
            gains(ui, &mut p.wb);
            ui.label(tl!("Sensor gains bypass the temperature slider range."));
        }
        if p.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Developing RAW profile preview…"));
            });
        } else if ui.button(tl!("Apply profile / white balance")).clicked() {
            action = Some("dcp-apply");
        }
        if let Some(raw) = &d.raw_open {
            ui.label(format!("{}: {}", tl!("Applied profile"), raw.camera_profile.as_ref().map(|p| p.name.as_str()).unwrap_or("Embedded / default")));
        }
        if let Some(error) = &p.error {
            ui.colored_label(Tokens::get(ui.ctx()).text, error);
        }
        ui.separator();
        ui.label(tl!("Create an infrared DCP"));
        ui.text_edit_singleline(&mut p.name);
        ui.label(tl!("Calibration shift (R / G / B)"));
        gains(ui, &mut p.calibration);
        ui.label(tl!("Start from the selected DCP or embedded RAW calibration."));
        if ui.button(tl!("Create DCP profile")).clicked() {
            action = Some("dcp-create");
        }
        if ui.add_enabled(p.selected.is_some(), egui::Button::new(tl!("Export selected DCP…"))).clicked() {
            action = Some("dcp-export");
        }
    });
    action
}

fn gains(ui: &mut egui::Ui, values: &mut [f64; 3]) {
    ui.horizontal(|ui| {
        for (label, value) in ["R", "G", "B"].into_iter().zip(values.iter_mut()) {
            ui.label(label);
            ui.add(egui::DragValue::new(value).range(0.0001..=10000.0).speed(0.01).max_decimals(4));
        }
    });
}

fn raw_bytes(raw: &RawOpen) -> Result<Arc<Vec<u8>>, String> {
    if let Some(bytes) = &raw.bytes {
        return Ok(bytes.clone());
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = &raw.path {
        return photocraft_format::read_file(std::path::Path::new(path)).map(Arc::new).map_err(|e| e.to_string());
    }
    Err("RAW source is unavailable".into())
}

fn store(app: &mut PhotocraftApp) {
    if let Some(d) = &app.camera_raw {
        let value = d.profiles.library.save();
        app.session.prefs.edit(|p| p.dialogs.insert(KEY.into(), value));
    }
}

type DcpFiles = Box<dyn Iterator<Item = (String, Result<Vec<u8>, String>)>>;

// Reading is lazy so bulk import holds at most one unvalidated file buffer.
fn picked_files(answer: crate::file_dialog::FileDialogAnswer) -> Result<DcpFiles, String> {
    use crate::file_dialog::FileDialogAnswer as Answer;
    Ok(match answer {
        Answer::Contents(name, bytes) => Box::new(std::iter::once((name, Ok(bytes)))),
        Answer::ContentsMany(files) => Box::new(files.into_iter().take(MAX_PROFILES).map(|(n, b)| (n, Ok(b)))),
        Answer::Paths(paths) => Box::new(paths.into_iter().take(MAX_PROFILES).map(|path| {
            let data = read_dcp(&path);
            (path, data)
        })),
        _ => return Err("expected DCP profile files".into()),
    })
}

pub(super) fn report(app: &mut PhotocraftApp, result: Result<Value, String>) {
    if let Err(error) = &result
        && error != crate::file_dialog::CANCELLED
        && let Some(d) = app.camera_raw.as_mut()
    {
        d.profiles.error = Some(error.clone());
    }
    report_file_error(app, result);
}

pub(super) fn import(app: &mut PhotocraftApp) -> Result<Value, String> {
    if app.camera_raw.as_ref().and_then(|d| d.raw_open.as_ref()).is_none() {
        return Err("DCP profiles require an open RAW file".into());
    }
    use crate::file_dialog::FileDialogRequest as Request;
    app.ask_file(Request::Open { multiple: true, initial_dir: None, extensions: Some(vec!["dcp".into()]) }, |app, answer| {
        let files = picked_files(answer)?;
        let d = app.camera_raw.as_mut().ok_or("Camera Raw is closed")?;
        let mut errors = Vec::new();
        let mut imported = 0;
        for (name, bytes) in files {
            match bytes.and_then(|b| d.profiles.library.import(&name, &b)) {
                Ok(i) => {
                    d.profiles.selected = Some(i);
                    imported += 1;
                }
                Err(e) => errors.push(format!("{}: {e}", crate::file_open::display_name(&name))),
            }
        }
        d.profiles.error = (!errors.is_empty()).then(|| errors.join("\n"));
        store(app);
        app.ui.status = format!("Imported {imported} DCP profiles");
        Ok(json!({"imported":imported,"errors":errors}))
    })
}

fn read_dcp(path: &str) -> Result<Vec<u8>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Read;
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take((MAX_FILE + 1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FILE {
            return Err("DCP file exceeds 16 MiB".into());
        }
        Ok(bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        Err("choose a DCP file through the browser picker".into())
    }
}

pub(super) fn create(app: &mut PhotocraftApp) -> Result<Value, String> {
    let d = app.camera_raw.as_ref().ok_or("Camera Raw is closed")?;
    let raw = d.raw_open.as_ref().ok_or("DCP creation requires a RAW file")?;
    let p = &d.profiles;
    let base = p.selected.and_then(|i| p.library.entries.get(i)).map(|e| e.bytes.as_slice());
    let bytes = if base.is_none() { raw_bytes(raw)? } else { Arc::new(Vec::new()) };
    let created = photocraft_io::raw::create_camera_profile(&bytes, base, &p.name, p.calibration).map_err(|e| e.to_string())?;
    let d = app.camera_raw.as_mut().ok_or("Camera Raw is closed")?;
    let i = d.profiles.library.import("Infrared custom.dcp", &created)?;
    d.profiles.selected = Some(i);
    d.profiles.error = None;
    store(app);
    Ok(json!({"created":i}))
}

pub(super) fn export(app: &mut PhotocraftApp) -> Result<Value, String> {
    let d = app.camera_raw.as_ref().ok_or("Camera Raw is closed")?;
    let e = d.profiles.selected.and_then(|i| d.profiles.library.entries.get(i)).ok_or("select a DCP profile first")?;
    let bytes = e.bytes.clone();
    app.pick_save("Infrared profile.dcp", move |app, path| {
        let write = app.services.write.as_mut().ok_or("no writer configured")?;
        write(&path, &bytes)?;
        Ok(json!({"path":path}))
    })
}

pub(super) fn apply(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let d = app.camera_raw.as_mut().ok_or("Camera Raw is closed")?;
    if d.profiles.pending.is_some() {
        return Err("a profile preview is already developing".into());
    }
    let mut raw = d.raw_open.clone().ok_or("DCP profiles require a RAW file")?;
    raw.camera_profile = d.profiles.selected.and_then(|i| d.profiles.library.entries.get(i)).map(|e| e.profile.clone());
    raw.white_balance_multipliers = d.profiles.custom_wb.then_some(d.profiles.wb);
    let (tx, rx) = mpsc::channel();
    let repaint = ctx.clone();
    let cancelled = d.profiles.cancelled.clone();
    let work = move || {
        let result = (|| {
            let _permit = PROFILE_DEVELOP.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if cancelled.load(Ordering::Acquire) {
                return Err("profile preview cancelled".into());
            }
            let bytes = raw_bytes(&raw)?;
            let tuning = photocraft_io::raw::RawTuning {
                camera_profile: raw.camera_profile.clone(),
                white_balance_multipliers: raw.white_balance_multipliers,
                demosaic: raw.demosaic,
                ..Default::default()
            };
            let (imported, _) = photocraft_io::raw::import_raw_tuned(&raw.name, &bytes, &tuning).map_err(|e| e.to_string())?;
            let surface = imported.document.layers.first().and_then(|l| l.surface()).cloned().ok_or("RAW developer returned no pixel layer")?;
            let curve = photocraft_io::raw::tone_curve_with_profile(&bytes, raw.camera_profile.as_deref()).map_err(|e| e.to_string())?;
            Ok(Preview { surface, curve, raw })
        })();
        let _ = tx.send(result);
        repaint.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("dcp-preview".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    d.profiles.pending = Some(rx);
    d.profiles.error = None;
    Ok(json!({"profilePreviewPending":true}))
}

pub(super) fn poll(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(d) = app.camera_raw.as_mut() else { return };
    let Some(rx) = &d.profiles.pending else { return };
    let result = match rx.try_recv() {
        Ok(r) => r,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => Err("DCP preview worker stopped".into()),
    };
    d.profiles.pending = None;
    let frame = match result {
        Ok(f) => f,
        Err(e) => {
            d.profiles.error = Some(e);
            return;
        }
    };
    let Some(index) = document_index(app, frame.raw.document) else {
        if let Some(d) = app.camera_raw.as_mut() {
            d.profiles.error = Some("the RAW document was closed".into());
        }
        return;
    };
    app.session.set_active(index);
    let Some(mut old) = app.camera_raw.take() else { return };
    let view = app.ui.camera_raw_scope.clone();
    let nav = app.ui.camera_raw_preview;
    let result = open_pixels(app, ctx, old.layer, frame.surface, old.params.clone(), Coverage::None, Target::OpenRaw);
    app.ui.camera_raw_scope = view;
    app.ui.camera_raw_preview = nav;
    match result {
        Ok(()) => {
            if let Some(next) = app.camera_raw.as_mut() {
                next.layer_name = old.layer_name;
                let mut raw = frame.raw;
                if let Some(current) = old.raw_open.as_ref() {
                    raw.demosaic = current.demosaic;
                }
                next.raw_open = Some(raw);
                let curve = Some(Arc::new(frame.curve));
                next.raw_curve = curve.clone();
                next.dirty = true;
                next.detail.raw_curve = curve;
                next.profiles = old.profiles;
                next.original = old.original;
                next.undo = old.undo;
                next.redo = old.redo;
                next.show_before = old.show_before;
            }
        }
        Err(e) => {
            old.profiles.error = Some(e);
            app.camera_raw = Some(old);
        }
    }
}

/// The control channel drives the same library and preview as the native controls.
pub(super) fn control(app: &mut PhotocraftApp, ctx: &egui::Context, value: &Value) -> Result<Value, String> {
    let fields = value.as_object().ok_or("profiles must be an object")?;
    for key in fields.keys() {
        if !["action", "index", "name", "whiteBalanceMultipliers", "calibrationMultipliers", "tutorialAction"].contains(&key.as_str()) {
            return Err(format!("unknown camera profiles property {key}"));
        }
    }
    let action = value.get("action").and_then(Value::as_str).ok_or("profiles.action is required")?;
    if !["describe", "select", "import", "apply", "create", "export", "tutorial"].contains(&action) {
        return Err("unknown camera profiles action".into());
    }
    let d = app.camera_raw.as_ref().ok_or("Camera Raw is closed")?;
    if action == "tutorial" {
        if fields.keys().any(|key| !["action", "tutorialAction"].contains(&key.as_str())) {
            return Err("tutorial accepts only action and tutorialAction".into());
        }
        let navigation = value.get("tutorialAction").and_then(Value::as_str).ok_or("tutorialAction is required")?;
        let d = app.camera_raw.as_mut().ok_or("Camera Raw is closed")?;
        d.profiles.tutorial.control(navigation)?;
        return Ok(d.profiles.describe(d.raw_open.as_ref()));
    }
    if value.get("tutorialAction").is_some() {
        return Err("tutorialAction requires action: tutorial".into());
    }
    if d.raw_open.is_none() {
        return Err("DCP profiles require a RAW file".into());
    }
    let mut selected = d.profiles.selected;
    if let Some(index) = value.get("index") {
        selected = if index.is_null() {
            None
        } else {
            Some(
                index
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .filter(|i| *i < d.profiles.library.entries.len())
                    .ok_or("profile index is outside the library")?,
            )
        };
    }
    let name = match value.get("name") {
        None => d.profiles.name.clone(),
        Some(n) => n.as_str().filter(|n| !n.trim().is_empty() && n.len() <= 255).ok_or("profile name must contain 1–255 bytes")?.to_string(),
    };
    let parse_gains = |v: &Value| -> Result<[f64; 3], String> {
        let a = v.as_array().filter(|a| a.len() == 3).ok_or("white balance needs three multipliers")?;
        let mut gains = [1.0; 3];
        for (out, input) in gains.iter_mut().zip(a) {
            *out =
                input.as_f64().filter(|v| v.is_finite() && (0.0001..=10000.0).contains(v)).ok_or("multipliers must be finite and between 0.0001 and 10000")?;
        }
        Ok(gains)
    };
    let wb = match value.get("whiteBalanceMultipliers") {
        None => d.profiles.custom_wb.then_some(d.profiles.wb),
        Some(v) if v.is_null() => None,
        Some(v) => Some(parse_gains(v)?),
    };
    let calibration = match value.get("calibrationMultipliers") {
        None => d.profiles.calibration,
        Some(v) => parse_gains(v)?,
    };
    let d = app.camera_raw.as_mut().ok_or("Camera Raw is closed")?;
    d.profiles.selected = selected;
    d.profiles.name = name;
    d.profiles.custom_wb = wb.is_some();
    if let Some(wb) = wb {
        d.profiles.wb = wb;
    }
    d.profiles.calibration = calibration;
    match action {
        "import" => import(app),
        "apply" => apply(app, ctx),
        "create" => create(app),
        "export" => export(app),
        _ => Ok(app.camera_raw.as_ref().map(|d| d.profiles.describe(d.raw_open.as_ref())).unwrap_or(Value::Null)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_paths_are_read_one_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.dcp");
        let second = dir.path().join("second.dcp");
        std::fs::write(&first, [1, 2, 3]).unwrap();
        let mut files =
            picked_files(crate::file_dialog::FileDialogAnswer::Paths(vec![first.to_string_lossy().into_owned(), second.to_string_lossy().into_owned()]))
                .unwrap();
        assert_eq!(files.next().unwrap().1.unwrap(), [1, 2, 3]);
        // This file did not exist when the first file was read. An eager batch
        // would already have tried to read it and retained all earlier buffers.
        std::fs::write(&second, [4, 5, 6]).unwrap();
        assert_eq!(files.next().unwrap().1.unwrap(), [4, 5, 6]);
        assert!(files.next().is_none());
    }

    #[test]
    fn malformed_profile_does_not_change_the_library() {
        let mut library = Library::default();
        assert!(library.import("broken.dcp", b"not a profile").is_err());
        assert!(library.entries.is_empty());
    }

    #[test]
    fn hostile_persisted_library_is_bounded() {
        let value = serde_json::json!([{"file":"bad.dcp","data":"!!!!"}]);
        assert!(Library::load(Some(&value)).entries.is_empty());
    }
}
