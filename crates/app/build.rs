fn main() {
    let v = env!("CARGO_PKG_VERSION");
    let mut parts = v.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    let bin = format!("SCHWAETZ_VERSION_BIN={a},{b},{c},0");
    let s = format!("SCHWAETZ_VERSION_STR=\"{v}\"");
    println!("cargo:rerun-if-changed=res/schwaetz.rc");
    println!("cargo:rerun-if-changed=res/schwaetz.ico");
    println!("cargo:rerun-if-changed=res/schwaetz.manifest");
    embed_resource::compile("res/schwaetz.rc", [bin.as_str(), s.as_str()]).manifest_required().unwrap();
}
