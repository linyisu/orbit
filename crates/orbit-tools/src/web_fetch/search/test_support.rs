use tokio::{io::AsyncReadExt, net::TcpStream};

/// Read complete mock HTTP requests, including POST bodies split across packets.
pub(super) async fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4_096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0, "request ended before its body was complete");
        bytes.extend_from_slice(&buffer[..count]);
        let Some(end) = bytes
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
        else {
            continue;
        };
        let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        if bytes.len() >= end + 4 + length {
            return (headers, bytes[end + 4..end + 4 + length].to_vec());
        }
    }
}
