// Embeds Windows version information (product name, version, copyright) into
// betterui.exe. SignPath requires every signed build to carry the project's
// product name and a product version, and checks them before signing.
// The version comes from `version` in Cargo.toml.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "betterUI");
    res.set("FileDescription", "betterUI");
    res.set("CompanyName", "Thundr-cod");
    res.set("OriginalFilename", "betterui.exe");
    res.set("InternalName", "betterui");
    res.set(
        "LegalCopyright",
        "Copyright (C) 2026 Thundr-cod. Licensed under GPL-3.0.",
    );
    res.compile().expect("failed to embed Windows version resource");
}
