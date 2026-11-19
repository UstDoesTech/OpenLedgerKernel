use std::io::Read;

/// Random bytes from the kernel, as hexadecimal.
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// An error message from a service, cut short and with anything that is not plain text removed,
/// so that an answer can never fill a log.
pub fn brief(text: &[u8]) -> String {
    let s: String = String::from_utf8_lossy(text)
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    s
}
