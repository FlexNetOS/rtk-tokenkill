use std::io::{self, Write};

fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| panic!("fixture mode is required"));
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    match mode.as_str() {
        "binary-failure" => {
            stdout.write_all(&[0xff, b'A']).expect("write stdout");
            stdout.flush().expect("flush stdout");
            stderr.write_all(&[0, b'B']).expect("write stderr");
            stderr.flush().expect("flush stderr");
            std::process::exit(7);
        }
        "partial" => {
            stdout.write_all(b"left").expect("write first stdout chunk");
            stdout.flush().expect("flush first stdout chunk");
            stderr.write_all(b"problem").expect("write stderr chunk");
            stderr.flush().expect("flush stderr chunk");
            stdout
                .write_all(b"right")
                .expect("write second stdout chunk");
            stdout.flush().expect("flush second stdout chunk");
        }
        "nuon" => {
            stdout.write_all(b"nuon").expect("write stdout");
            stdout.flush().expect("flush stdout");
        }
        other => panic!("unknown fixture mode: {other}"),
    }
}
