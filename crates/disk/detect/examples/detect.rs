fn main() {
    for path in std::env::args().skip(1) {
        match disk_detect::detect_path(path.as_ref()) {
            Ok(d) => println!("{path}: {d}"),
            Err(e) => println!("{path}: {e}"),
        }
    }
}
