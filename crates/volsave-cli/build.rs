//! Embeds the Windows manifest and version information into `volsave.exe`.
//!
//! Without them the file is a bare, unsigned program, which antivirus
//! heuristics (Windows Defender among them) tend to treat as suspicious.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=volsave.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("volsave.manifest");
    let rc = out.join("volsave.rc");
    fs::write(&rc, resource_script(&manifest)).expect("cannot write the resource script");
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_required()
        .expect("cannot embed the Windows resources");
}

/// Manifest plus VERSIONINFO, as `windres`/`rc` read it.
fn resource_script(manifest: &std::path::Path) -> String {
    let version = |name: &str| env::var(format!("CARGO_PKG_VERSION_{name}")).unwrap_or_default();
    let (major, minor, patch) = (version("MAJOR"), version("MINOR"), version("PATCH"));
    let text = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // Resource compilers take forward slashes on every host.
    let manifest = manifest.to_string_lossy().replace('\\', "/");
    format!(
        r#"#define RT_MANIFEST 24
1 RT_MANIFEST "{manifest}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "VolSave"
      VALUE "FileDescription", "VolSave disk and volume imaging for Windows"
      VALUE "FileVersion", "{text}"
      VALUE "InternalName", "volsave"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "volsave.exe"
      VALUE "ProductName", "VolSave"
      VALUE "ProductVersion", "{text}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}
