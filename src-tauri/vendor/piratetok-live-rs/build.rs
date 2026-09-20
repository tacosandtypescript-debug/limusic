// The crate's own build script ran a source lint that failed on its own published source —
// on Windows because its exemption list used forward slashes and Path yields backslashes, and
// everywhere because it scans src/bin/, which a library consumer never compiles. It is neutered
// here rather than removed, because Cargo auto-detects a build script by file name and a missing
// one it was told about is a hard error.
//
// 0BSD permits this: the licence grants the right to modify, and the vendored copy is checked in.
fn main() {}

