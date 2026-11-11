#[path = "../scripts/brand_build.rs"]
mod brand_build;
fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let _ = brand_build::configure(root);
}
