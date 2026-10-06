use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) async fn read_request(reader: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        while !request_complete(&bytes).expect("valid test HTTP request") {
            let count = reader.read(&mut buffer).await.expect("read request");
            assert_ne!(
                count, 0,
                "connection closed before the request was complete"
            );
            bytes.extend_from_slice(&buffer[..count]);
        }
        bytes
    })
    .await
    .expect("test HTTP request did not complete within 30 seconds")
}

fn request_complete(bytes: &[u8]) -> Result<bool, &'static str> {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(false);
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "invalid headers")?;
    let mut length = None;
    let mut chunked = false;
    for header in headers.lines().skip(1) {
        let (name, value) = header.split_once(':').ok_or("invalid header field")?;
        if name.eq_ignore_ascii_case("content-length") {
            let value = value
                .trim()
                .parse::<usize>()
                .map_err(|_| "invalid content length")?;
            if length.replace(value).is_some() {
                return Err("duplicate content length");
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err("unsupported transfer encoding");
            }
            chunked = true;
        }
    }
    let body = &bytes[header_end + 4..];
    if chunked {
        if length.is_some() {
            return Err("ambiguous request framing");
        }
        chunks_complete(body)
    } else {
        Ok(body.len() >= length.unwrap_or(0))
    }
}

fn chunks_complete(mut bytes: &[u8]) -> Result<bool, &'static str> {
    loop {
        let Some(line) = take_line(&mut bytes) else {
            return Ok(false);
        };
        let size = line.split(|byte| *byte == b';').next().unwrap();
        let size = std::str::from_utf8(size)
            .map_err(|_| "invalid chunk size")?
            .trim();
        if size.is_empty() || !size.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid chunk size");
        }
        let size = usize::from_str_radix(size, 16).map_err(|_| "invalid chunk size")?;
        if size == 0 {
            while let Some(trailer) = take_line(&mut bytes) {
                if trailer.is_empty() {
                    return Ok(true);
                }
                if !trailer.contains(&b':') {
                    return Err("invalid trailer");
                }
            }
            return Ok(false);
        }
        let end = size.checked_add(2).ok_or("chunk size overflow")?;
        if bytes.len() < end {
            return Ok(false);
        }
        if &bytes[size..end] != b"\r\n" {
            return Err("missing chunk terminator");
        }
        bytes = &bytes[end..];
    }
}

fn take_line<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
    let end = bytes.windows(2).position(|window| window == b"\r\n")?;
    let line = &bytes[..end];
    *bytes = &bytes[end + 2..];
    Some(line)
}

#[test]
fn every_prefix_waits_for_complete_request_framing() {
    for request in [
        &b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"[..],
        b"POST / HTTP/1.1\r\ncOnTeNt-LeNgTh: 5\r\n\r\nhello",
        b"POST / HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n0\r\n\r\n\r\n0\r\n\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding: Chunked\r\n\r\n2;foo=bar\r\nhe\r\n3\r\nllo\r\n0;done=yes\r\nX-Result: ok\r\n\r\n",
    ] {
        for end in 0..request.len() {
            assert_eq!(request_complete(&request[..end]), Ok(false), "prefix {end}: {request:?}");
        }
        assert_eq!(request_complete(request), Ok(true));
    }
}

#[test]
fn invalid_framing_is_reported_instead_of_waiting_for_more_bytes() {
    for request in [
        &b"POST / HTTP/1.1\r\nContent-Length: invalid\r\n\r\n"[..],
        b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\ng\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nxXX",
    ] {
        assert!(request_complete(request).is_err(), "{request:?}");
    }
}

#[tokio::test]
async fn both_servers_wait_for_headers_and_body_sent_separately() {
    use tokio::{io::AsyncWriteExt, net::TcpStream, time::timeout};

    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    for chunked in [false, true] {
        for held in [false, true] {
            let (url, captured) = if held {
                let (url, captured) =
                    super::test_server_held(vec![response.to_vec()], Duration::ZERO).await;
                (url, tokio::spawn(captured))
            } else {
                let (url, count) = super::test_server_responses(vec![response.to_vec()]).await;
                (
                    url,
                    tokio::spawn(async move {
                        assert_eq!(count.await.unwrap(), 1);
                        String::new()
                    }),
                )
            };
            let mut socket = TcpStream::connect(url.strip_prefix("http://").unwrap())
                .await
                .unwrap();
            let header = if chunked {
                "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"
            } else {
                "POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\n"
            };
            let fragments: &[&[u8]] = if chunked {
                &[
                    b"2\r\nhe\r\n",
                    b"3\r\nllo\r\n",
                    b"0\r\nX-Result: ok\r\n\r\n",
                ]
            } else {
                &[b"he", b"ll", b"o"]
            };
            socket.write_all(header.as_bytes()).await.unwrap();
            for fragment in fragments {
                let mut byte = [0];
                assert!(
                    timeout(Duration::from_millis(50), socket.read(&mut byte))
                        .await
                        .is_err(),
                    "server replied before the whole body arrived (held={held}, chunked={chunked})"
                );
                socket.write_all(fragment).await.unwrap();
            }
            let mut received = Vec::new();
            timeout(Duration::from_secs(5), socket.read_to_end(&mut received))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(received, response);
            let captured = timeout(Duration::from_secs(5), captured)
                .await
                .unwrap()
                .unwrap();
            if held {
                let expected = [header.as_bytes(), &fragments.concat()].concat();
                assert_eq!(captured.as_bytes(), expected.to_ascii_lowercase());
            }
        }
    }
}
