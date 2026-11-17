//! The reference producer for the conformance suite: `reference-producer
//! --socket PATH [--push] [--heads N] [--size WxH]`.

use origami_display::v4::sys::UnixListener;

fn main() {
    let mut path = None;
    let (mut push, mut heads, mut w, mut h) = (false, 1u32, 256u32, 128u32);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => path = args.next(),
            "--push" => push = true,
            "--heads" => heads = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--size" => {
                let v = args.next().unwrap_or_default();
                if let Some((a, b)) = v.split_once('x') {
                    (w, h) = (a.parse().unwrap_or(w), b.parse().unwrap_or(h));
                }
            }
            _ => {
                eprintln!(
                    "usage: reference-producer --socket PATH [--push] [--heads N] [--size WxH]"
                );
                std::process::exit(2);
            }
        }
    }
    let Some(path) = path else {
        eprintln!("reference-producer: --socket PATH is required");
        std::process::exit(2);
    };
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind socket");
    let p = origami_display_producer::reference::start(listener, heads, w, h, push)
        .expect("start producer");
    eprintln!("reference-producer: listening on {path}");
    loop {
        std::thread::park();
        let _ = &p;
    }
}
