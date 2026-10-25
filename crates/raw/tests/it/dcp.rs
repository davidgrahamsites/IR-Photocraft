//! Synthetic camera profiles, written from the DNG format description.
use photocraft_raw::testgen::{DngSpec, TiffBuilder, Val};
use photocraft_raw::*;
use std::sync::Arc;

fn profile(big_endian: bool, extra: Vec<(u16, Val)>) -> Vec<u8> {
    let mut w = TiffBuilder { big_endian, ..Default::default() };
    let mut tags = vec![
        (50708, Val::Ascii("Synthetic IR Camera".into())),
        (50936, Val::Ascii("Infrared custom".into())),
        (50778, Val::Short(vec![21])),
        (50721, Val::SRational(vec![(1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1)])),
    ];
    for (tag, value) in extra {
        tags.retain(|(t, _)| *t != tag);
        tags.push((tag, value));
    }
    let i = w.ifd(tags);
    w.chain.push(i);
    let mut b = w.build();
    let magic = if big_endian { 0x4352u16.to_be_bytes() } else { 0x4352u16.to_le_bytes() };
    b[2..4].copy_from_slice(&magic);
    // TiffBuilder's Undefined is used to supply format-specified FLOAT payloads.
    let count = if big_endian { u16::from_be_bytes([b[8], b[9]]) } else { u16::from_le_bytes([b[8], b[9]]) };
    for i in 0..count as usize {
        let at = 10 + i * 12;
        let tag = if big_endian { u16::from_be_bytes([b[at], b[at + 1]]) } else { u16::from_le_bytes([b[at], b[at + 1]]) };
        if [50938, 50939, 50940, 50982].contains(&tag) {
            let count =
                if big_endian { u32::from_be_bytes(b[at + 4..at + 8].try_into().unwrap()) } else { u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) }
                    / 4;
            let typ = if big_endian { 11u16.to_be_bytes() } else { 11u16.to_le_bytes() };
            let count = if big_endian { count.to_be_bytes() } else { count.to_le_bytes() };
            b[at + 2..at + 4].copy_from_slice(&typ);
            b[at + 4..at + 8].copy_from_slice(&count);
        }
    }
    b
}

#[test]
fn profiles_roundtrip_and_match_only_their_camera() {
    for be in [false, true] {
        let p = CameraProfile::parse(&profile(be, vec![])).unwrap();
        assert_eq!(p.name, "Infrared custom");
        assert!(p.matches_camera(None, Some("Synthetic IR Camera")));
        assert!(!p.matches_camera(None, Some("A different camera")));
        let again = CameraProfile::parse(&p.to_dcp().unwrap()).unwrap();
        assert_eq!(again, p);
    }
}

#[test]
fn malformed_profiles_are_rejected_without_partial_acceptance() {
    let p = profile(false, vec![]);
    for cut in 0..p.len() {
        assert!(CameraProfile::parse(&p[..cut]).is_err(), "truncated at {cut}");
    }
    let mut bad = p.clone();
    bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(CameraProfile::parse(&bad).is_err());
    // An incomplete hue/saturation map must not be treated as a matrix-only profile.
    assert!(CameraProfile::parse(&profile(false, vec![(50937, Val::Long(vec![6, 2, 1]))])).is_err());
}

#[test]
fn infrared_profile_changes_raw_development_and_exports() {
    let b = DngSpec::cfa(16, 16, vec![12000; 256]).build();
    let mut s = decode(&b, &Limits::default()).unwrap();
    s.model = Some("Synthetic IR Camera".into());
    s.camera_wb = Some([1.0; 3]);
    s.color.calibrations = vec![Calibration {
        temperature: 6504.0,
        color_matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        forward_matrix: None,
        camera_calibration: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    }];
    for (i, v) in s.data.iter_mut().enumerate() {
        *v = match (i / 16 % 2, i % 2) {
            (0, 0) => 16000,
            (1, 1) => 4000,
            _ => 12000,
        };
    }
    let original = s.data.clone();
    let base = CameraProfile::from_sensor(&s, "IR base").unwrap();
    let shifted = base.clone().with_white_balance_multipliers([0.25, 1.0, 2.0]).unwrap();
    let shifted = CameraProfile::parse(&shifted.to_dcp().unwrap()).unwrap();
    let opts = DevelopOptions { camera_profile: Some(Arc::new(base)), ..Default::default() };
    let a = develop_sensor(&s, &opts).unwrap();
    let opts = DevelopOptions { camera_profile: Some(Arc::new(shifted)), ..opts };
    let b = develop_sensor(&s, &opts).unwrap();
    assert_ne!(a.rgb, b.rgb);
    assert_eq!(s.data, original);
    assert!(CameraProfile::from_sensor(&s, "IR").unwrap().with_white_balance_multipliers([0.0, 1.0, 1.0]).is_err());
}

#[test]
fn map_and_tone_profiles_export_without_losing_rendering_stages() {
    for be in [false, true] {
        let floats = |v: &[f32]| Val::Undefined(v.iter().flat_map(|v| if be { v.to_be_bytes() } else { v.to_le_bytes() }).collect());
        let bytes = profile(
            be,
            vec![
                (50937, Val::Long(vec![1, 2, 1])),
                (50938, floats(&[0.0, 1.0, 1.0, 30.0, 0.8, 1.0])),
                (50981, Val::Long(vec![1, 2, 1])),
                (50982, floats(&[0.0, 1.0, 1.0, -30.0, 1.2, 1.0])),
                (50940, floats(&[0.0, 0.0, 0.5, 0.7, 1.0, 1.0])),
                (50942, Val::Ascii("User-created infrared profile".into())),
            ],
        );
        let p = CameraProfile::parse(&bytes).unwrap();
        assert_eq!(CameraProfile::parse(&p.to_dcp().unwrap()).unwrap(), p);
        let curve = p.sampled_tone_curve();
        assert_eq!(curve.len(), 1025);
        assert_eq!(curve.first(), Some(&[0.0, 0.0]));
        assert_eq!(curve.last(), Some(&[1.0, 1.0]));
    }
}

#[test]
fn explicit_infrared_white_balance_supports_wide_positive_gains() {
    let bytes = DngSpec::cfa(16, 16, vec![100; 256]).build();
    let s = decode(&bytes, &Limits::default()).unwrap();
    for gains in [[0.1, 1.0, 0.2], [100_000.0, 1.0, 0.0001]] {
        let result = develop_sensor(&s, &DevelopOptions { white_balance: WhiteBalance::Multipliers(gains), ..Default::default() });
        assert!(result.is_ok(), "{gains:?}: {result:?}");
    }
    for gains in [[0.0, 1.0, 1.0], [f64::INFINITY, 1.0, 1.0], [f64::NAN, 1.0, 1.0], [1e100, 1.0, 1.0]] {
        assert!(develop_sensor(&s, &DevelopOptions { white_balance: WhiteBalance::Multipliers(gains), ..Default::default() }).is_err());
    }
}

#[test]
fn identity_ir_gains_preserve_the_base_forward_matrix_and_profile() {
    let p = CameraProfile::parse(&profile(
        false,
        vec![
            (50964, Val::SRational(vec![(9642, 10000), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (8249, 10000)])),
            (50932, Val::Ascii("User calibration".into())),
        ],
    ))
    .unwrap();
    assert_eq!(p.clone().with_white_balance_multipliers([1.0; 3]).unwrap(), p);
    assert_eq!(p.clone().with_white_balance_multipliers([4.0; 3]).unwrap(), p);
}

#[test]
fn creating_from_a_measured_raw_curve_adds_required_dcp_endpoints() {
    let bytes = DngSpec::cfa(16, 16, vec![1000; 256]).build();
    let mut sensor = decode(&bytes, &Limits::default()).unwrap();
    sensor.model = Some("Synthetic IR Camera".into());
    CameraProfile::parse(&profile(false, vec![])).unwrap().apply(&mut sensor).unwrap();
    sensor.tone_curve = vec![[0.01, 0.02], [0.5, 0.7], [1.0, 1.0]];
    let p = CameraProfile::from_sensor(&sensor, "Created IR").unwrap();
    let exported = p.to_dcp().unwrap();
    assert_eq!(CameraProfile::parse(&exported).unwrap().tone_curve().first(), Some(&[0.0, 0.0]));
}
