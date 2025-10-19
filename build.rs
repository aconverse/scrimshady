fn main() {
    // Embed the manifest on Windows
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        embed_resource::compile("scrimshady.rc", embed_resource::NONE);
    }
}
