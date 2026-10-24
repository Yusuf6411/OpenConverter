//! Release helpers: SBOM, checksums and a Debian package. Everything here is a
//! development tool; none of it is linked into a shipped binary.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::process::Command;

/// The workspace version (xtask has its own, unrelated one).
fn version() -> String {
    metadata()
        .ok()
        .and_then(|m| {
            m["packages"]
                .as_array()?
                .iter()
                .find(|p| p["name"] == "oc-cli")
                .and_then(|p| p["version"].as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| "0.0.0".into())
}

fn metadata() -> Result<Value, String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

/// CycloneDX 1.5 list of every package in the resolved tree except the dev tool.
pub fn sbom(out: &Path) -> Result<(), String> {
    let meta = metadata()?;
    let mut components = Vec::new();
    for p in meta["packages"].as_array().into_iter().flatten() {
        let name = p["name"].as_str().unwrap_or("");
        if name == "xtask" {
            continue;
        }
        let version = p["version"].as_str().unwrap_or("");
        let mut c = json!({
            "type": "library",
            "name": name,
            "version": version,
            "purl": format!("pkg:cargo/{name}@{version}"),
        });
        if let Some(l) = p["license"].as_str() {
            c["licenses"] = json!([{"expression": l}]);
        }
        components.push(c);
    }
    components.sort_by_key(|c| {
        (
            c["name"].as_str().map(str::to_owned),
            c["version"].as_str().map(str::to_owned),
        )
    });
    let doc = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {"component": {"type": "application", "name": "openconverter", "version": version()}},
        "components": components,
    });
    fs::write(
        out,
        serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("sbom: {} components -> {}", components.len(), out.display());
    Ok(())
}

/// Writes `SHA256SUMS` next to the files in `dir` (all regular files except itself).
pub fn checksums(dir: &Path) -> Result<(), String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != "SHA256SUMS")
        .collect();
    names.sort();
    let mut text = String::new();
    for n in &names {
        let data = fs::read(dir.join(n)).map_err(|e| e.to_string())?;
        let hex: String = Sha256::digest(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        text.push_str(&format!("{hex}  {n}\n"));
    }
    fs::write(dir.join("SHA256SUMS"), text).map_err(|e| e.to_string())?;
    println!("checksums: {} files", names.len());
    Ok(())
}

/// `deb <out-dir> <cli-binary> [desktop-binary]`
pub fn deb(out_dir: &Path, cli: &Path, desktop: Option<&Path>) -> Result<(), String> {
    let version = version();
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let stage = out_dir.join("deb-stage");
    let _ = fs::remove_dir_all(&stage);
    let bin = stage.join("usr/bin");
    let share = stage.join("usr/share");
    fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    fs::create_dir_all(stage.join("DEBIAN")).map_err(|e| e.to_string())?;
    fs::create_dir_all(share.join("applications")).map_err(|e| e.to_string())?;
    fs::create_dir_all(share.join("icons/hicolor/128x128/apps")).map_err(|e| e.to_string())?;
    fs::create_dir_all(share.join("doc/openconverter")).map_err(|e| e.to_string())?;
    fs::copy(cli, bin.join("openconverter")).map_err(|e| e.to_string())?;
    let mut depends = String::from("libc6");
    if let Some(d) = desktop {
        fs::copy(d, bin.join("openconverter-desktop")).map_err(|e| e.to_string())?;
        depends = "libc6, libwebkit2gtk-4.1-0, libgtk-3-0".into();
        fs::write(
            share.join("applications/openconverter.desktop"),
            "[Desktop Entry]\nType=Application\nName=OpenConverter\nComment=Convert files on your own computer\nExec=openconverter-desktop\nIcon=openconverter\nCategories=Utility;AudioVideo;Graphics;\nTerminal=false\n",
        )
        .map_err(|e| e.to_string())?;
        fs::copy(
            "crates/oc-desktop/icons/128x128.png",
            share.join("icons/hicolor/128x128/apps/openconverter.png"),
        )
        .map_err(|e| e.to_string())?;
    }
    fs::copy("LICENSE", share.join("doc/openconverter/copyright")).map_err(|e| e.to_string())?;
    let size_kb: u64 = walk_size(&stage) / 1024 + 1;
    fs::write(
        stage.join("DEBIAN/control"),
        format!(
            "Package: openconverter\nVersion: {}\nSection: utils\nPriority: optional\nArchitecture: {arch}\nDepends: {depends}\nInstalled-Size: {size_kb}\nMaintainer: OpenConverter contributors <noreply@users.noreply.github.com>\nHomepage: https://github.com/Yusuf6411/OpenConverter\nDescription: Private file converter\n Converts images, audio, video, documents and more entirely on your own\n computer. No uploads, no accounts, no network.\n",
            version.replace("-dev", "~dev")
        ),
    )
    .map_err(|e| e.to_string())?;
    let out = out_dir.join(format!(
        "openconverter_{}_{arch}.deb",
        version.replace("-dev", "~dev")
    ));
    let st = Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&stage)
        .arg(&out)
        .status()
        .map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&stage);
    if !st.success() {
        return Err("dpkg-deb failed".into());
    }
    println!("deb: {}", out.display());
    Ok(())
}

fn walk_size(p: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                walk_size(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
