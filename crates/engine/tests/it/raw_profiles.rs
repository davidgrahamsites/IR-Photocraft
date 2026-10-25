//! Custom infrared DCP creation and raw-stage profile development.

use serde_json::json;

#[test]
fn ir_profile_commands_are_registered_and_reject_bad_params_without_mutation() {
    let mut session = photocraft_engine::Session::new();
    for id in ["raw.profile.inspect", "raw.profile.create", "raw.open"] {
        let error = session.execute(id, serde_json::json!({})).unwrap_err().to_string();
        assert!(!error.contains("unknown command"), "{id}: {error}");
        assert!(error.contains("path") || error.contains("sourcePath"), "{id}: {error}");
        assert!(session.documents().is_empty());
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn ir_profile_create_inspect_open_and_overwrite_guard() {
    use photocraft_raw::testgen::DngSpec;
    let dir = tempfile::tempdir().unwrap();
    let raw_path = dir.path().join("infrared.dng");
    let dcp_path = dir.path().join("720nm.dcp");
    let mut raw = DngSpec::cfa(16, 16, vec![5000; 256]);
    raw.color_matrix1 = Some((21, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]));
    std::fs::write(&raw_path, raw.build()).unwrap();
    let mut session = photocraft_engine::Session::new();
    let params = json!({"sourcePath": raw_path, "path": dcp_path, "name": "Infrared 720nm", "whiteBalanceMultipliers": [0.25, 1.0, 3.0]});
    let created = session.execute("raw.profile.create", params.clone()).unwrap();
    assert_eq!(created["name"], "Infrared 720nm");
    let first = std::fs::read(&dcp_path).unwrap();
    assert!(session.execute("raw.profile.create", params).is_err());
    assert_eq!(std::fs::read(&dcp_path).unwrap(), first, "overwriting requires explicit authorization");
    let inspected = session.execute("raw.profile.inspect", json!({"path": dcp_path})).unwrap();
    assert_eq!(inspected["name"], "Infrared 720nm");
    let opened = session.execute("raw.open", json!({"path": raw_path, "profilePath": dcp_path, "whiteBalanceMultipliers": [0.15, 1.0, 4.0]})).unwrap();
    assert!(opened["whiteBalanceApplied"].as_bool().unwrap());
    assert_eq!(session.documents().len(), 1);
}
