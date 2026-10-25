//! User-supplied DNG camera profiles (`.dcp`), from the Adobe DNG Specification 1.7.1,
//! Camera Profiles and Mapping Camera Color Space to CIE XYZ Space. No vendor profiles or
//! implementation code are included. This product includes DNG technology under license by Adobe.
//!
//! Metadata is bounded and parsed without I/O. A selected profile replaces the camera's default
//! rendering; unsupported rendering stages fail explicitly rather than producing a partial look.

use crate::Sensor;
use crate::color::{self, Calibration, ColorInfo, IDENTITY, Mat3};
use crate::error::{RawError, Result};
use crate::tiff::{Ifd, Tiff, tag};

pub const MAX_DCP_BYTES: usize = 32 * 1024 * 1024;
const MAX_TABLE_VALUES: usize = 1 << 20;
const PROFILE_NAME: u16 = 50936;
const TONE_CURVE: u16 = 50940;

/// A parsed custom camera profile. Sensor-specific profiles are only applicable to their
/// recorded camera model; imported profile data remains user-owned and is never bundled.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraProfile {
    pub name: String,
    pub camera_model: Option<String>,
    calibrations: Vec<Calibration>,
    illuminants: Vec<u32>,
    tone: Vec<[f32; 2]>,
    baseline_exposure: f64,
    hue_sat: Vec<Table>,
    look: Option<Table>,
    copyright: Option<String>,
    embed_policy: u32,
    signature: Option<String>,
    black_render: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct Table {
    dims: [usize; 3],    // hue, saturation, value; saturation varies fastest
    data: Vec<[f32; 3]>, // hue shift (degrees), saturation scale, value scale
    encoding: u32,       // 0: linear, 1: sRGB value-axis encoding
}

fn malformed(msg: impl Into<String>) -> RawError {
    RawError::Malformed(format!("DCP profile: {}", msg.into()))
}

fn text(t: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<Option<String>> {
    let Some(e) = ifd.get(tag) else { return Ok(None) };
    if !matches!(e.typ, 1 | 2) || e.count == 0 || e.count > 4096 {
        return Err(malformed(format!("invalid text tag {tag}")));
    }
    let b = t.raw(e).ok_or_else(|| malformed("truncated text"))?;
    let raw = b.strip_suffix(&[0]).ok_or_else(|| malformed("text is not NUL-terminated"))?;
    let s = std::str::from_utf8(raw).map_err(|_| malformed("invalid UTF-8 text"))?;
    if s.contains('\0') {
        return Err(malformed("text contains embedded NUL"));
    }
    Ok((!s.trim().is_empty()).then(|| s.trim().to_string()))
}

fn floats(t: &Tiff<'_>, ifd: &Ifd, tag: u16, count: usize) -> Result<Option<Vec<f64>>> {
    let Some(e) = ifd.get(tag) else { return Ok(None) };
    if e.count as usize != count || count > MAX_TABLE_VALUES {
        return Err(malformed(format!("tag {tag} has invalid count")));
    }
    let v = t.floats(e);
    if v.len() != count || v.iter().any(|v| !v.is_finite()) {
        return Err(malformed(format!("tag {tag} contains invalid numbers")));
    }
    Ok(Some(v))
}

fn matrix(t: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<Option<Mat3>> {
    let Some(v) = floats(t, ifd, tag, 9)? else { return Ok(None) };
    let m = color::from_slice(&v).ok_or_else(|| malformed("invalid matrix"))?;
    if color::invert(&m).is_none() || m.iter().flatten().any(|v| v.abs() > 1e6) {
        return Err(malformed(format!("tag {tag} has a singular or unbounded matrix")));
    }
    Ok(Some(m))
}

fn scalar(t: &Tiff<'_>, ifd: &Ifd, tag: u16, default: f64) -> Result<f64> {
    Ok(floats(t, ifd, tag, 1)?.and_then(|v| v.first().copied()).unwrap_or(default))
}

fn table(t: &Tiff<'_>, ifd: &Ifd, dims_tag: u16, data_tag: u16, encoding_tag: u16) -> Result<Option<Table>> {
    let (dims, data) = (ifd.get(dims_tag), ifd.get(data_tag));
    let (Some(dims), Some(data)) = (dims, data) else {
        if dims.is_some() || data.is_some() {
            return Err(malformed("incomplete hue/saturation table"));
        }
        return Ok(None);
    };
    if dims.count != 3 || dims.typ != 4 {
        return Err(malformed("invalid hue/saturation table dimensions"));
    }
    let ds = t.uints(dims);
    let [h, s, v] = ds.as_slice() else { return Err(malformed("truncated table dimensions")) };
    if *h == 0 || *s < 2 || *v == 0 {
        return Err(malformed("hue/saturation table dimensions are zero or too small"));
    }
    let n = u64::from(*h).checked_mul(u64::from(*s)).and_then(|n| n.checked_mul(u64::from(*v))).and_then(|n| n.checked_mul(3));
    let Some(n) = n.filter(|n| *n <= MAX_TABLE_VALUES as u64) else {
        return Err(RawError::LimitExceeded("DCP hue/saturation table".into()));
    };
    if data.typ != 11 || u64::from(data.count) != n {
        return Err(malformed("hue/saturation table data count does not match dimensions"));
    }
    let values = t.floats(data);
    if values.len() != n as usize || values.iter().any(|v| !v.is_finite() || v.abs() > 1e6) {
        return Err(malformed("invalid hue/saturation table values"));
    }
    let mut data = Vec::with_capacity(values.len() / 3);
    for chunk in values.chunks_exact(3) {
        let (Some(h), Some(s), Some(v)) = (chunk.first(), chunk.get(1), chunk.get(2)) else { return Err(malformed("truncated table entry")) };
        if *s < 0.0 || *v < 0.0 {
            return Err(malformed("negative hue/saturation table scale"));
        }
        if data.len() % (*ds.get(1).unwrap_or(&2) as usize) == 0 && (*v - 1.0).abs() > 1e-6 {
            return Err(malformed("zero-saturation table entries must have value scale 1"));
        }
        data.push([*h as f32, *s as f32, *v as f32]);
    }
    let encoding = scalar(t, ifd, encoding_tag, 0.0)?;
    if encoding != 0.0 && encoding != 1.0 {
        return Err(RawError::unsupported("DCP table encoding"));
    }
    Ok(Some(Table { dims: [*h as usize, *s as usize, *v as usize], data, encoding: encoding as u32 }))
}

impl CameraProfile {
    /// Parses a standalone DCP (either byte order). Ordinary TIFF headers are also accepted,
    /// but a complete camera-profile matrix set is required. At most 32 MiB is accepted.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_DCP_BYTES {
            return Err(RawError::LimitExceeded("DCP profile larger than 32 MiB".into()));
        }
        let le = match bytes.get(..4) {
            Some(b"IIRC" | b"II*\0") => true,
            Some(b"MMCR" | b"MM\0*") => false,
            _ => return Err(malformed("not a DNG camera profile")),
        };
        let mut t = Tiff { data: bytes, le, first_ifd: 0 };
        t.first_ifd = t.u32_at(4).ok_or_else(|| malformed("truncated header"))? as usize;
        if t.first_ifd < 8 {
            return Err(malformed("profile directory overlaps its header"));
        }
        let count = t.u16_at(t.first_ifd).ok_or_else(|| malformed("missing directory"))? as usize;
        if count == 0 || count > 256 {
            return Err(malformed("invalid profile directory size"));
        }
        let dir_end = t.first_ifd.checked_add(2 + count * 12).ok_or_else(|| malformed("directory overflow"))?;
        let next = t.u32_at(dir_end).ok_or_else(|| malformed("truncated directory"))?;
        if next != 0 {
            return Err(RawError::unsupported("DCP multiple profile directories"));
        }
        let ifd = t.ifd_at(t.first_ifd, 0).ok_or_else(|| malformed("invalid directory"))?;
        if ifd.entries.len() != count {
            return Err(malformed("truncated directory"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for e in &ifd.entries {
            if !seen.insert(e.tag) || e.unit() == 0 || e.count as usize > MAX_TABLE_VALUES || t.raw(e).is_none() {
                return Err(malformed("duplicate, truncated or oversized profile tag"));
            }
        }
        // These rendering stages cannot be approximated by a global camera matrix or HSV map.
        // Refuse them rather than silently changing the user's infrared rendering.
        for tag in [50725, 50726, 52525, 52529, 52530, 52531, 52532, 52533, 52534, 52535, 52536, 52543, 52544] {
            if ifd.has(tag) {
                return Err(RawError::unsupported(format!("DCP rendering tag {tag} (gain/RGB tables, custom/third illuminant or multi-channel reduction)")));
            }
        }
        if let Some(e) = ifd.get(52551) {
            if e.typ != 7 || e.count != 8 || t.u16_at(e.at) != Some(1) {
                return Err(malformed("invalid profile dynamic range tag"));
            }
            if t.u16_at(e.at.saturating_add(2)) != Some(0) {
                return Err(RawError::unsupported("DCP HDR ProfileDynamicRange"));
            }
        }
        let name = text(&t, &ifd, PROFILE_NAME)?.unwrap_or_else(|| "Unnamed camera profile".into());
        let camera_model = text(&t, &ifd, tag::UNIQUE_CAMERA_MODEL)?;
        let mut calibrations = Vec::new();
        let mut illuminants = Vec::new();
        for (cm, fm, ill) in [
            (tag::COLOR_MATRIX_1, tag::FORWARD_MATRIX_1, tag::CALIBRATION_ILLUMINANT_1),
            (tag::COLOR_MATRIX_2, tag::FORWARD_MATRIX_2, tag::CALIBRATION_ILLUMINANT_2),
        ] {
            let Some(color_matrix) = matrix(&t, &ifd, cm)? else {
                if ifd.has(fm) || ifd.has(ill) || (cm == tag::COLOR_MATRIX_1 && ifd.has(tag::COLOR_MATRIX_2)) {
                    return Err(malformed("incomplete calibration matrix set"));
                }
                continue;
            };
            let code = scalar(&t, &ifd, ill, 0.0)?;
            if code.fract() != 0.0 || !(1.0..=255.0).contains(&code) {
                return Err(malformed("invalid calibration illuminant"));
            }
            let temperature = color::illuminant_temperature(code as u32).ok_or_else(|| RawError::unsupported(format!("DCP illuminant {code}")))?;
            calibrations.push(Calibration { temperature, color_matrix, forward_matrix: matrix(&t, &ifd, fm)?, camera_calibration: IDENTITY });
            illuminants.push(code as u32);
        }
        if calibrations.is_empty() {
            return Err(malformed("profile has no ColorMatrix1"));
        }
        if calibrations.len() == 2 && calibrations.first().zip(calibrations.get(1)).is_some_and(|(a, b)| a.temperature == b.temperature) {
            return Err(malformed("dual profile illuminants are identical"));
        }
        let mut hue_sat = Vec::new();
        if let Some(tab) = table(&t, &ifd, 50937, 50938, 51107)? {
            hue_sat.push(tab);
            if ifd.has(50939)
                && let Some(tab) = table(&t, &ifd, 50937, 50939, 51107)?
            {
                hue_sat.push(tab);
            }
        } else if ifd.has(50939) {
            return Err(malformed("second hue map has no first table"));
        }
        if hue_sat.len() > calibrations.len() {
            return Err(malformed("second hue map has no second calibration"));
        }
        if calibrations.first().zip(calibrations.get(1)).is_some_and(|(a, b)| a.temperature > b.temperature) {
            calibrations.reverse();
            illuminants.reverse();
            if hue_sat.len() == 2 {
                hue_sat.reverse();
            }
        }
        let mut tone = Vec::new();
        if let Some(e) = ifd.get(TONE_CURVE) {
            if e.typ != 11 || e.count < 4 || e.count > 8192 || e.count % 2 != 0 {
                return Err(malformed("invalid profile tone curve"));
            }
            let values = t.floats(e);
            if values.len() != e.count as usize {
                return Err(malformed("truncated profile tone curve"));
            }
            for pair in values.chunks_exact(2) {
                let (Some(x), Some(y)) = (pair.first(), pair.get(1)) else { return Err(malformed("invalid tone point")) };
                if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) || tone.last().is_some_and(|p: &[f32; 2]| *x as f32 <= p[0]) {
                    return Err(malformed("tone curve inputs must increase and values must lie within 0..1"));
                }
                tone.push([*x as f32, *y as f32]);
            }
            if tone.first() != Some(&[0.0, 0.0]) || tone.last() != Some(&[1.0, 1.0]) {
                return Err(malformed("tone curve endpoints must be (0,0) and (1,1)"));
            }
        }
        let baseline_exposure = scalar(&t, &ifd, 51109, 0.0)?;
        if !(-10.0..=10.0).contains(&baseline_exposure) {
            return Err(malformed("baseline exposure offset exceeds ten stops"));
        }
        let embed = scalar(&t, &ifd, 50941, 0.0)?;
        if embed.fract() != 0.0 || !(0.0..=3.0).contains(&embed) {
            return Err(malformed("invalid profile embed policy"));
        }
        let black = scalar(&t, &ifd, 51110, 0.0)?;
        if black != 0.0 && black != 1.0 {
            return Err(RawError::unsupported("DCP default black rendering mode"));
        }
        Ok(Self {
            name,
            camera_model,
            calibrations,
            illuminants,
            tone,
            baseline_exposure,
            hue_sat,
            look: table(&t, &ifd, 50981, 50982, 51108)?,
            copyright: text(&t, &ifd, 50942)?,
            embed_policy: embed as u32,
            signature: text(&t, &ifd, 50932)?,
            black_render: black as u32,
        })
    }

    /// The profile's linear-light tone knots. Empty means linear, replacing any camera default.
    pub fn tone_curve(&self) -> &[[f32; 2]] {
        &self.tone
    }

    /// Model matching ignores ASCII case and whitespace but never uses prefix/substring matches.
    pub fn matches_camera(&self, make: Option<&str>, model: Option<&str>) -> bool {
        fn key(s: &str) -> String {
            s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
        }
        let Some(expected) = &self.camera_model else { return true };
        let Some(model) = model else { return false };
        if model.len() > 4095 || make.is_some_and(|m| m.len() > 4095) {
            return false;
        }
        let expected = key(expected);
        if expected == key(model) {
            return true;
        }
        make.is_some_and(|make| {
            let make = key(make);
            let short = [" corporation", " corp.", " corp", " inc.", " inc", " co., ltd.", " co.,ltd."]
                .iter()
                .find_map(|suffix| make.strip_suffix(suffix))
                .unwrap_or(&make);
            expected == key(&format!("{make} {model}")) || expected == key(&format!("{short} {model}"))
        })
    }

    /// Copies only the profile's calibration, exposure and tone metadata into a decoded sensor.
    /// Use `DevelopOptions::camera_profile` for normal development: it also applies HSV maps.
    /// Do not call this and then pass the same profile in the options (exposure would be doubled).
    pub fn apply(&self, sensor: &mut Sensor) -> Result<()> {
        self.validate_sensor(sensor)?;
        if !self.hue_sat.is_empty() || self.look.is_some() {
            return Err(RawError::unsupported("this DCP has HSV tables: select it through DevelopOptions::camera_profile so every stage is applied"));
        }
        sensor.color = self.color_info(&sensor.color);
        sensor.tone_curve = self.tone.clone();
        sensor.baseline_exposure += self.baseline_exposure;
        if self.black_render == 1 {
            sensor.black_point = 0.0;
        }
        sensor.warnings.push(format!("custom camera profile: {}", self.name));
        Ok(())
    }

    pub(crate) fn validate_sensor(&self, sensor: &Sensor) -> Result<()> {
        if sensor.is_monochrome() {
            return Err(RawError::unsupported("a colour DCP profile cannot be applied to a monochrome sensor"));
        }
        if !self.matches_camera(sensor.make.as_deref(), sensor.model.as_deref()) {
            return Err(RawError::unsupported(format!(
                "DCP profile '{}' is for {}, not {}",
                self.name,
                self.camera_model.as_deref().unwrap_or("an unspecified camera"),
                sensor.model.as_deref().unwrap_or("an unidentified camera")
            )));
        }
        Ok(())
    }

    pub(crate) fn color_info(&self, source: &ColorInfo) -> ColorInfo {
        // An external profile has its own reference camera. Sensor CameraCalibration matrices
        // are only valid with a matching signature; the Sensor model does not retain that
        // signature, so use identity rather than accidentally mixing two calibrations.
        ColorInfo { calibrations: self.calibrations.clone(), ..source.clone() }
    }

    /// Starts an editable profile from a raw's existing colour calibration. A calibration
    /// cannot be recovered from an arbitrary photo when the raw contains none.
    pub fn from_sensor(sensor: &Sensor, name: &str) -> Result<Self> {
        if name.trim().is_empty() || name.len() > 4095 || name.contains('\0') {
            return Err(malformed("profile name is empty, too long or contains NUL"));
        }
        if sensor.color.calibrations.is_empty() || sensor.color.calibrations.len() > 2 || sensor.is_monochrome() {
            return Err(RawError::unsupported(
                "creating a DCP requires an existing colour calibration; import a base DCP or convert the raw to a calibrated DNG first",
            ));
        }
        if sensor.model.as_deref().is_none_or(|m| m.trim().is_empty() || m.len() > 4095 || m.contains('\0')) {
            return Err(malformed("creating a camera-specific profile requires a valid camera model (at most 4095 bytes)"));
        }
        let mut cal = sensor.color.calibrations.clone();
        cal.sort_by(|a, b| a.temperature.total_cmp(&b.temperature));
        let codes = [17, 18, 19, 20, 21, 22, 23, 24, 3, 1, 4, 9, 10, 11, 12, 13, 14, 15, 16, 2];
        let mut illuminants = Vec::new();
        for c in &mut cal {
            let code = codes
                .iter()
                .find(|code| color::illuminant_temperature(**code).is_some_and(|t| (t - c.temperature).abs() < 2.0))
                .ok_or_else(|| RawError::unsupported("creating a DCP for a custom illuminant"))?;
            if c.camera_calibration != IDENTITY {
                c.color_matrix = color::mul(&c.camera_calibration, &c.color_matrix);
                // Absorbing a non-diagonal per-camera calibration changes FM's reference domain.
                c.forward_matrix = None;
                c.camera_calibration = IDENTITY;
            }
            if color::invert(&c.color_matrix).is_none() {
                return Err(malformed("singular source calibration"));
            }
            illuminants.push(*code);
        }
        // Decoder-measured curves may omit the zero knot and rely on extrapolation through
        // the origin. A standalone SDR DCP must state both endpoints explicitly.
        let mut tone = sensor.tone_curve.clone();
        if !tone.is_empty() {
            if tone.iter().any(|p| p.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))) || tone.windows(2).any(|p| p[0][0] >= p[1][0]) {
                return Err(malformed("source tone curve is invalid"));
            }
            if tone.first().is_some_and(|p| p[0] > 0.0) {
                tone.insert(0, [0.0, 0.0]);
            }
            if tone.last().is_some_and(|p| p[0] < 1.0) {
                tone.push([1.0, 1.0]);
            }
            if tone.first() != Some(&[0.0, 0.0]) || tone.last() != Some(&[1.0, 1.0]) {
                return Err(malformed("source tone curve cannot be represented as an SDR DCP"));
            }
        }
        Ok(Self {
            name: name.trim().to_string(),
            camera_model: sensor.model.clone(),
            calibrations: cal,
            illuminants,
            tone,
            baseline_exposure: 0.0,
            hue_sat: Vec::new(),
            look: None,
            copyright: None,
            embed_policy: 3,
            signature: None,
            black_render: 1,
        })
    }

    /// Adjusts the reference camera's R/G/B response by positive relative gains. This is useful
    /// for authoring infrared profiles from an existing base; it is not a spectral measurement.
    /// Gains are normalized to green. ForwardMatrix is removed so the adjusted ColorMatrix
    /// drives the new mapping. Tone and look tables are preserved.
    pub fn with_white_balance_multipliers(mut self, gains: [f64; 3]) -> Result<Self> {
        if gains.iter().any(|v| !v.is_finite() || !(1e-4..=1e4).contains(v)) {
            return Err(malformed("IR response gains must be finite and within 0.0001..10000"));
        }
        let gains = gains.map(|v| v / gains[1]);
        if gains.iter().any(|v| !(1e-4..=1e4).contains(v)) {
            return Err(malformed("relative IR gains exceed 0.0001..10000"));
        }
        if gains == [1.0; 3] {
            return Ok(self);
        }
        for c in &mut self.calibrations {
            c.color_matrix = color::mul(&color::diag(gains), &c.color_matrix);
            c.forward_matrix = None;
            c.camera_calibration = IDENTITY;
        }
        self.signature = None;
        Ok(self)
    }

    pub(crate) fn black_point(&self, camera_default: f32) -> f32 {
        if self.black_render == 1 { 0.0 } else { camera_default }
    }

    pub(crate) fn exposure_offset(&self) -> f64 {
        self.baseline_exposure
    }

    pub(crate) fn map_weight(&self, info: &ColorInfo, neutral: [f64; 3]) -> f32 {
        let Some((a, b)) = self.calibrations.first().zip(self.calibrations.get(1)) else { return 1.0 };
        let t = color::cct(info.neutral_to_xy(neutral));
        ((1.0 / t - 1.0 / b.temperature) / (1.0 / a.temperature - 1.0 / b.temperature)).clamp(0.0, 1.0) as f32
    }

    pub(crate) fn map_color(&self, rgb: [f32; 3], weight: f32) -> [f32; 3] {
        self.hue_sat.first().map_or(rgb, |map| map.apply(rgb, self.hue_sat.get(1), weight))
    }

    pub(crate) fn apply_look(&self, rgb: [f32; 3]) -> [f32; 3] {
        self.look.as_ref().map_or(rgb, |map| map.apply(rgb, None, 1.0))
    }

    /// Writes a standalone little-endian DCP with the public DNG profile tags. Copyright and
    /// embedding-policy metadata survive import/export; no vendor profiles are installed.
    pub fn to_dcp(&self) -> Result<Vec<u8>> {
        if self.name.trim().is_empty() || self.name.len() > 4095 || self.name.contains('\0') {
            return Err(malformed("invalid profile name"));
        }
        if self.camera_model.as_deref().is_some_and(|m| m.len() > 4095 || m.contains('\0')) {
            return Err(malformed("invalid profile camera model"));
        }
        let mut tags = Vec::new();
        let ascii = |s: &str| {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            b
        };
        if let Some(model) = &self.camera_model {
            tags.push((50708, 2, ascii(model)));
        }
        tags.push((PROFILE_NAME, 2, ascii(&self.name)));
        for (i, c) in self.calibrations.iter().enumerate() {
            let cm_tag = if i == 0 { 50721 } else { 50722 };
            tags.push((cm_tag, 10, rationals(c.color_matrix.into_iter().flatten())?));
            if let Some(fm) = c.forward_matrix {
                tags.push((if i == 0 { 50964 } else { 50965 }, 10, rationals(fm.into_iter().flatten())?));
            }
            let ill = self.illuminants.get(i).copied().ok_or_else(|| malformed("missing export illuminant"))?;
            tags.push((if i == 0 { 50778 } else { 50779 }, 3, (ill as u16).to_le_bytes().to_vec()));
        }
        if !self.tone.is_empty() {
            tags.push((TONE_CURVE, 11, self.tone.iter().flatten().flat_map(|v| v.to_le_bytes()).collect()));
        }
        if let Some(first) = self.hue_sat.first() {
            tags.push((50937, 4, first.dims.iter().flat_map(|v| (*v as u32).to_le_bytes()).collect()));
            tags.push((50938, 11, first.bytes()));
            if let Some(second) = self.hue_sat.get(1) {
                tags.push((50939, 11, second.bytes()));
            }
            tags.push((51107, 4, first.encoding.to_le_bytes().to_vec()));
        }
        if let Some(look) = &self.look {
            tags.push((50981, 4, look.dims.iter().flat_map(|v| (*v as u32).to_le_bytes()).collect()));
            tags.push((50982, 11, look.bytes()));
            tags.push((51108, 4, look.encoding.to_le_bytes().to_vec()));
        }
        if self.baseline_exposure != 0.0 {
            tags.push((51109, 10, rationals([self.baseline_exposure])?));
        }
        tags.push((50941, 4, self.embed_policy.to_le_bytes().to_vec()));
        if let Some(c) = &self.copyright {
            tags.push((50942, 2, ascii(c)));
        }
        if let Some(s) = &self.signature {
            tags.push((50932, 2, ascii(s)));
        }
        // User-authored profiles do not inherit the camera default's adaptive black subtraction.
        tags.push((51110, 4, self.black_render.to_le_bytes().to_vec()));
        tags.sort_by_key(|(tag, _, _)| *tag);
        let n = u16::try_from(tags.len()).map_err(|_| malformed("too many export tags"))?;
        let mut b = b"IIRC\x08\0\0\0".to_vec();
        b.extend_from_slice(&n.to_le_bytes());
        let first_value = 8 + 2 + tags.len() * 12 + 4;
        let mut payload = Vec::new();
        for (tag, typ, value) in tags {
            let unit = crate::tiff::type_size(typ);
            let count = u32::try_from(value.len() / unit).map_err(|_| malformed("export count overflow"))?;
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&typ.to_le_bytes());
            b.extend_from_slice(&count.to_le_bytes());
            if value.len() <= 4 {
                b.extend_from_slice(&value);
                b.resize(b.len() + 4 - value.len(), 0);
            } else {
                let offset = u32::try_from(first_value + payload.len()).map_err(|_| malformed("export offset overflow"))?;
                b.extend_from_slice(&offset.to_le_bytes());
                payload.extend_from_slice(&value);
                if payload.len() % 2 != 0 {
                    payload.push(0);
                }
            }
        }
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend(payload);
        if b.len() > MAX_DCP_BYTES {
            return Err(RawError::LimitExceeded("DCP export larger than 32 MiB".into()));
        }
        // The writer and reader share the same validation boundary.
        Self::parse(&b)?;
        Ok(b)
    }
}

fn rationals(values: impl IntoIterator<Item = f64>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for v in values {
        if !v.is_finite() || v.abs() > 1e6 {
            return Err(malformed("matrix coefficient is too large to export"));
        }
        let denominator = (f64::from(i32::MAX) / v.abs().max(1.0)).floor().clamp(1.0, 1_000_000.0) as i32;
        let n = (v * f64::from(denominator)).round() as i32;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&denominator.to_le_bytes());
    }
    Ok(out)
}

impl Table {
    fn bytes(&self) -> Vec<u8> {
        self.data.iter().flatten().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn sample(&self, h: f32, s: f32, v: f32) -> [f32; 3] {
        let [nh, ns, nv] = self.dims;
        let h = h.rem_euclid(6.0) * nh as f32 / 6.0;
        let s = s.clamp(0.0, 1.0) * (ns - 1) as f32;
        let v = v.clamp(0.0, 1.0) * (nv - 1) as f32;
        let hi = (h.floor() as usize).min(nh - 1);
        let si = (s.floor() as usize).min(ns - 2);
        let vi = (v.floor() as usize).min(nv.saturating_sub(2));
        let mut out = [0.0; 3];
        for (hh, wh) in [(hi, 1.0 - h.fract()), ((hi + 1) % nh, h.fract())] {
            for (ss, ws) in [(si, 1.0 - (s - si as f32)), (si + 1, s - si as f32)] {
                for (vv, wv) in [(vi, if nv == 1 { 1.0 } else { 1.0 - (v - vi as f32) }), ((vi + 1).min(nv - 1), if nv == 1 { 0.0 } else { v - vi as f32 })] {
                    let i = (vv * nh + hh) * ns + ss;
                    let entry = self.data.get(i).copied().unwrap_or([0.0, 1.0, 1.0]);
                    for (o, e) in out.iter_mut().zip(entry) {
                        *o += e * wh * ws * wv;
                    }
                }
            }
        }
        out
    }

    fn apply(&self, rgb: [f32; 3], other: Option<&Table>, weight: f32) -> [f32; 3] {
        let [r, g, b] = rgb.map(|v| v.max(0.0));
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max <= 0.0 {
            return [0.0; 3];
        }
        let d = max - min;
        let mut hue = if d <= 0.0 {
            0.0
        } else if max == r {
            (g - b) / d
        } else if max == g {
            2.0 + (b - r) / d
        } else {
            4.0 + (r - g) / d
        };
        hue = hue.rem_euclid(6.0);
        let mut sat = d / max;
        let encoded = self.encoding == 1 && self.dims[2] > 1;
        let mut value = if encoded { encode_srgb(max) } else { max };
        let mut effect = self.sample(hue, sat, value);
        if let Some(other) = other {
            let second = other.sample(hue, sat, value);
            for (a, b) in effect.iter_mut().zip(second) {
                *a = *a * weight + b * (1.0 - weight);
            }
        }
        hue = (hue + effect[0] / 60.0).rem_euclid(6.0);
        sat = (sat * effect[1]).clamp(0.0, 1.0);
        value = (value * effect[2]).min(1.0);
        if encoded {
            value = decode_srgb(value);
        }
        let chroma = value * sat;
        let x = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
        let m = value - chroma;
        let v = match hue as u32 {
            0 => [chroma, x, 0.0],
            1 => [x, chroma, 0.0],
            2 => [0.0, chroma, x],
            3 => [0.0, x, chroma],
            4 => [x, 0.0, chroma],
            _ => [chroma, 0.0, x],
        };
        v.map(|v| v + m)
    }
}
fn encode_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}
fn decode_srgb(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Natural cubic spline for the profile's linear-light tone knots (DNG ProfileToneCurve).
/// The tridiagonal solve is performed once, before building the development gamma LUT.
pub(crate) struct ProfileSpline<'a> {
    points: &'a [[f32; 2]],
    second: Vec<f64>,
}
impl<'a> ProfileSpline<'a> {
    pub(crate) fn new(points: &'a [[f32; 2]]) -> Self {
        let n = points.len();
        let mut second = vec![0.0; n];
        let mut upper = vec![0.0; n];
        let mut rhs = vec![0.0; n];
        for i in 1..n.saturating_sub(1) {
            let Some(([x0, y0], [x1, y1], [x2, y2])) = points.get(i - 1).zip(points.get(i)).zip(points.get(i + 1)).map(|((a, b), c)| (*a, *b, *c)) else {
                continue;
            };
            let a = f64::from(x1 - x0);
            let c = f64::from(x2 - x1);
            let b = 2.0 * (a + c);
            let value = 6.0 * (f64::from(y2 - y1) / c - f64::from(y1 - y0) / a);
            let prev_u = upper.get(i - 1).copied().unwrap_or(0.0);
            let prev_r = rhs.get(i - 1).copied().unwrap_or(0.0);
            let denom = b - a * prev_u;
            if denom <= 0.0 || !denom.is_finite() {
                continue;
            }
            if let Some(u) = upper.get_mut(i) {
                *u = c / denom;
            }
            if let Some(r) = rhs.get_mut(i) {
                *r = (value - a * prev_r) / denom;
            }
        }
        for i in (1..n.saturating_sub(1)).rev() {
            let value = rhs.get(i).copied().unwrap_or(0.0) - upper.get(i).copied().unwrap_or(0.0) * second.get(i + 1).copied().unwrap_or(0.0);
            if let Some(s) = second.get_mut(i) {
                *s = value;
            }
        }
        Self { points, second }
    }
    pub(crate) fn eval(&self, x: f32) -> f32 {
        if self.points.len() < 2 {
            return x;
        }
        let i = self.points.partition_point(|p| p[0] <= x).clamp(1, self.points.len() - 1);
        let Some(([x0, y0], [x1, y1])) = self.points.get(i - 1).zip(self.points.get(i)).map(|(a, b)| (*a, *b)) else { return x };
        let h = f64::from(x1 - x0);
        if h <= 0.0 {
            return x;
        }
        let b = f64::from(x - x0) / h;
        let a = 1.0 - b;
        let s0 = self.second.get(i - 1).copied().unwrap_or(0.0);
        let s1 = self.second.get(i).copied().unwrap_or(0.0);
        (a * f64::from(y0) + b * f64::from(y1) + ((a * a * a - a) * s0 + (b * b * b - b) * s1) * h * h / 6.0).clamp(0.0, 1.0) as f32
    }
}

impl CameraProfile {
    /// A dense representation of the DCP cubic tone curve, for proxy re-toning paths whose
    /// interpolation is log-linear. Export keeps the original, unsampled curve knots.
    pub fn sampled_tone_curve(&self) -> Vec<[f32; 2]> {
        if self.tone.is_empty() {
            return Vec::new();
        }
        let spline = ProfileSpline::new(&self.tone);
        (0..=1024)
            .map(|i| {
                let x = (i as f32 / 1024.0).powi(2);
                [x, spline.eval(x)]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_maps_wrap_hue_and_interpolate_saturation() {
        let t = Table { dims: [1, 2, 1], data: vec![[0.0, 1.0, 1.0], [120.0, 1.0, 1.0]], encoding: 0 };
        let green = t.apply([0.8, 0.0, 0.0], None, 1.0);
        assert!((green[0]).abs() < 1e-6 && (green[1] - 0.8).abs() < 1e-6 && green[2].abs() < 1e-6);
        assert_eq!(t.apply([0.3; 3], None, 1.0), [0.3; 3]);
        let red = t.apply([0.0, 0.0, 0.8], None, 1.0);
        assert!((red[0] - 0.8).abs() < 1e-6 && red[1].abs() < 1e-6 && red[2].abs() < 1e-6);
    }

    #[test]
    fn map_value_encoding_and_dual_interpolation_follow_profile_rules() {
        let a = Table { dims: [1, 2, 2], data: vec![[0.0, 1.0, 1.0], [0.0, 1.0, 0.5], [0.0, 1.0, 1.0], [0.0, 1.0, 0.5]], encoding: 1 };
        let v = a.apply([0.21404114, 0.0, 0.0], None, 1.0);
        assert!((v[0] - decode_srgb(0.25)).abs() < 1e-6);
        let b = Table { dims: [1, 2, 1], data: vec![[0.0, 1.0, 1.0], [120.0, 1.0, 1.0]], encoding: 0 };
        let a = Table { dims: [1, 2, 1], data: vec![[0.0, 1.0, 1.0], [0.0, 1.0, 1.0]], encoding: 0 };
        let yellow = a.apply([0.8, 0.0, 0.0], Some(&b), 0.5);
        assert!((yellow[0] - 0.8).abs() < 1e-6 && (yellow[1] - 0.8).abs() < 1e-6 && yellow[2].abs() < 1e-6);
    }

    #[test]
    fn cubic_tone_hits_knots_and_preserves_linear_curve() {
        let linear = ProfileSpline::new(&[[0.0, 0.0], [0.2, 0.2], [1.0, 1.0]]);
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert!((linear.eval(x) - x).abs() < 1e-6);
        }
        let points = [[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]];
        let curve = ProfileSpline::new(&points);
        assert!((curve.eval(0.5) - 0.7).abs() < 1e-6);
        // Natural cubic spline differs from straight-line interpolation (0.35).
        assert!((curve.eval(0.25) - 0.3875).abs() < 1e-6);
    }
}
