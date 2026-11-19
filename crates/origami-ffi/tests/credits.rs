//! The credits document the apps read.

#[test]
fn the_credits_document_is_not_an_error() {
    let doc: serde_json::Value =
        serde_json::from_str(&origami_ffi::origami_credits_json()).unwrap();
    assert!(doc["error"].is_null() && !doc["licences"].as_array().unwrap().is_empty());
}
