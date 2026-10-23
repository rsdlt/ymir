//! ymir never writes to `~/.claude` or to a repository. This keeps any new
//! write from slipping in unnoticed: the code outside tests may not call
//! anything that creates, changes or removes files, except in the modules
//! allowed here, each with its reason.

use std::fs;
use std::path::Path;

/// Calls that write to the filesystem.
const WRITES: [&str; 10] = [
    "File::create",
    "OpenOptions",
    "File::options",
    "fs::write",
    "fs::remove",
    "fs::rename",
    "fs::copy",
    "fs::create_dir",
    "fs::hard_link",
    "set_permissions",
];

/// Modules that may write, and why.
const ALLOWED: [(&str, &str); 1] = [
    // Builds synthetic sessions in temporary directories, for tests only.
    ("testutil.rs", "test helpers"),
];

#[test]
fn only_allowed_modules_write_files() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for entry in fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if ALLOWED.iter().any(|(file, _)| *file == name) {
            continue;
        }
        let code = fs::read_to_string(&path).unwrap();
        // A module's tests sit at its end, after `#[cfg(test)]`, and may
        // set up files of their own.
        let shipped = code.split("#[cfg(test)]").next().unwrap();
        for (n, line) in shipped.lines().enumerate() {
            let line = line.trim_start();
            if line.starts_with("//") {
                continue;
            }
            for call in WRITES {
                if line.contains(call) {
                    found.push(format!("src/{name}:{}: {call}", n + 1));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "writes outside the allowed modules:\n{}",
        found.join("\n")
    );
}
