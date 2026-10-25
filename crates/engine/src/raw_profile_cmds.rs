//! Custom DCP calibration and infrared raw development. Byte-level work lives in io/raw;
//! these commands supply the native file-system boundary for the CLI and trusted desktop.

use std::sync::Arc;

use photocraft_io::raw::{CameraProfile, RawTuning};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::file_cmds::{native, read_file, str_param, write_file};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, message: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: message.into() }
}

fn optional_path<'a>(p: &'a Value, key: &str, cmd: &str) -> Result<Option<&'a str>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(path)) if !path.trim().is_empty() => Ok(Some(path)),
        _ => Err(bad(cmd, format!("`{key}` must be a nonempty path"))),
    }
}

fn multipliers(p: &Value, cmd: &str) -> Result<Option<[f64; 3]>> {
    let Some(value) = p.get("whiteBalanceMultipliers") else { return Ok(None) };
    let gains: [f64; 3] = serde_json::from_value(value.clone()).map_err(|_| bad(cmd, "whiteBalanceMultipliers must be [red, green, blue]"))?;
    if gains.iter().any(|gain| !gain.is_finite() || !(1e-6..=1e6).contains(gain)) {
        return Err(bad(cmd, "whiteBalanceMultipliers must be finite values in 0.000001–1000000"));
    }
    Ok(Some(gains))
}

/// DCPs are small metadata files, so cap their read before allocating, including a file that
/// grows while it is being read. The raw-image reader has its own larger documented budget.
#[cfg(not(target_arch = "wasm32"))]
fn read_profile(path: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let metadata = file.metadata().map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let maximum = photocraft_io::raw::MAX_DCP_BYTES;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(EngineError::Other("DCP profile must be a regular file no larger than 32 MiB".into()));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    if bytes.len() > maximum {
        return Err(EngineError::Other("DCP profile grew beyond 32 MiB while reading".into()));
    }
    Ok(bytes)
}

#[cfg(target_arch = "wasm32")]
fn read_profile(path: &str) -> Result<Vec<u8>> {
    read_file(path)
}

fn inspect(_s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_param(p, "path", "raw.profile.inspect")?;
    let bytes = read_profile(path)?;
    let profile = CameraProfile::parse(&bytes).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({"path": path, "name": profile.name, "cameraModel": profile.camera_model, "bytes": bytes.len()}))
}

fn create(_s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "raw.profile.create";
    let path = str_param(p, "path", CMD)?;
    if !std::path::Path::new(path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("dcp")) {
        return Err(bad(CMD, "output path must have a .dcp extension"));
    }
    let name = str_param(p, "name", CMD)?;
    let source = optional_path(p, "sourcePath", CMD)?;
    let base = optional_path(p, "profilePath", CMD)?;
    if source.is_none() && base.is_none() {
        return Err(bad(CMD, "provide sourcePath (a calibrated raw) or profilePath (a base DCP)"));
    }
    let gains = multipliers(p, CMD)?.unwrap_or([1.0; 3]);
    let overwrite = match p.get("overwrite") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(bad(CMD, "overwrite must be a boolean")),
    };
    #[cfg(not(target_arch = "wasm32"))]
    if std::path::Path::new(path).try_exists().map_err(|e| EngineError::Other(format!("{path}: {e}")))? && !overwrite {
        return Err(bad(CMD, "output already exists; choose another path or pass overwrite: true"));
    }
    #[cfg(target_arch = "wasm32")]
    let _ = overwrite;
    let base_bytes = base.map(read_profile).transpose()?;
    // With a base DCP, sourcePath is optional and is not read: its calibration is not used.
    let raw_bytes = if base_bytes.is_none() { source.map(read_file).transpose()?.unwrap_or_default() } else { Vec::new() };
    let bytes = photocraft_io::raw::create_camera_profile(&raw_bytes, base_bytes.as_deref(), name, gains).map_err(|e| EngineError::Other(e.to_string()))?;
    let profile = CameraProfile::parse(&bytes).map_err(|e| EngineError::Other(e.to_string()))?;
    write_file(path, &bytes)?;
    Ok(json!({"path": path, "name": profile.name, "cameraModel": profile.camera_model, "bytes": bytes.len()}))
}

fn open(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "raw.open";
    let path = str_param(p, "path", CMD)?;
    let profile_path = optional_path(p, "profilePath", CMD)?;
    let white_balance_multipliers = multipliers(p, CMD)?;
    let exposure = match p.get("exposure") {
        None => 0.0,
        Some(v) => v.as_f64().filter(|v| v.is_finite() && (-10.0..=10.0).contains(v)).ok_or_else(|| bad(CMD, "exposure must be finite EV in -10–10"))?,
    };
    let camera_profile =
        profile_path.map(|path| CameraProfile::parse(&read_profile(path)?).map(Arc::new).map_err(|e| EngineError::Other(e.to_string()))).transpose()?;
    let bytes = read_file(path)?;
    let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("Camera Raw");
    let tuning = RawTuning {
        camera_profile,
        white_balance_multipliers,
        exposure: exposure as f32,
        demosaic: s.prefs().raw_defaults.demosaic.method(),
        ..Default::default()
    };
    // This path deliberately has no embedded-JPEG fallback: that would discard the imported
    // calibration and pretend a rendered preview was a successful infrared raw development.
    let (r, wb_applied) = photocraft_io::raw::import_raw_tuned(name, &bytes, &tuning).map_err(|e| EngineError::Other(e.to_string()))?;
    let mut doc = r.document;
    let resolution = s.prefs().raw_defaults.resolution;
    if resolution.is_finite() {
        doc.resolution_dpi = resolution.clamp(1.0, 10_000.0) as f32;
    }
    let (document, color) = s.open_document(doc, None);
    if let Some(st) = s.active_mut() {
        st.source_read_only = true;
    }
    Ok(
        json!({"document": document, "color": color, "sourcePath": path, "profilePath": profile_path, "whiteBalanceApplied": wb_applied, "warnings": r.warnings}),
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "raw.profile.inspect",
            label: "Inspect Camera Profile",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str (.dcp)}"#,
            enabled: native,
            run: inspect,
            journal: false,
        },
        CommandSpec {
            id: "raw.profile.create",
            label: "Create Infrared Camera Profile",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str (.dcp output),"name":str,"sourcePath":str? (calibrated raw),"profilePath":str? (base DCP),"whiteBalanceMultipliers":[r,g,b]=[1,1,1] (calibration shift, each and relative to green 0.0001..10000),"overwrite":bool=false}"#,
            enabled: native,
            run: create,
            journal: true,
        },
        CommandSpec {
            id: "raw.open",
            label: "Open Infrared Raw",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str (raw),"profilePath":str? (.dcp),"whiteBalanceMultipliers":[r,g,b]? (0.000001..1000000),"exposure":EV=0 (-10..10)}"#,
            enabled: native,
            run: open,
            journal: true,
        },
    ]
}
