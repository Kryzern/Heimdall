// Shared by the build scripts of `heimdall` and `heimdall-indexer`: embeds the
// icon and version information, so Explorer, Task Manager and Settings > Apps
// show a proper name and icon instead of a blank executable.

fn embed_resources() {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    // The service only ever shows a small icon, so it skips the 256 px one.
    let ico = if env("CARGO_PKG_NAME") == "heimdall-indexer" { "heimdall-small.ico" } else { "heimdall.ico" };
    let icon = std::path::Path::new(&env("CARGO_MANIFEST_DIR")).join("../../packaging").join(ico);
    let version = env("CARGO_PKG_VERSION");
    let mut nums = version.split(['.', '-', '+']).map(|n| n.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0), nums.next().unwrap_or(0));
    let name = env("CARGO_PKG_NAME");
    let desc = env("CARGO_PKG_DESCRIPTION").replace('"', "'");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Ethan Dsouza (Kryzern)"
      VALUE "FileDescription", "{desc}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{name}"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "{name}.exe"
      VALUE "ProductName", "Heimdall"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "/"),
    );
    let out = std::path::Path::new(&env("OUT_DIR")).join("resources.rc");
    std::fs::write(&out, rc).expect("write resources.rc");
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().expect("compile resources");
    println!("cargo:rerun-if-changed=../../packaging/{ico}");
    println!("cargo:rerun-if-changed=../../packaging/resources.rs");
}
