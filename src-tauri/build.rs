fn main() {
    // The Google OAuth client is baked in at build time (see scripts/release.ps1).
    println!("cargo:rerun-if-env-changed=JEEM_GOOGLE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=JEEM_GOOGLE_CLIENT_SECRET");
    tauri_build::build()
}
