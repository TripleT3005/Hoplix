use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::test_support::test_password;

// Mock server

#[derive(Debug, Clone, Default)]
struct Recorded {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    early: bool,
    connection: usize,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

type Handler = Arc<dyn Fn(&Recorded) -> Vec<u8> + Send + Sync>;

struct Mock {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
    connections: Arc<AtomicUsize>,
}

impl Mock {
    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("poisoned").clone()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

async fn mock(handler: impl Fn(&Recorded) -> Vec<u8> + Send + Sync + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind failed");
    let address = listener.local_addr().expect("no address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let connections = Arc::new(AtomicUsize::new(0));
    let handler: Handler = Arc::new(handler);
    let (log, count) = (Arc::clone(&requests), Arc::clone(&connections));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let number = count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(serve(
                stream,
                number,
                Arc::clone(&handler),
                Arc::clone(&log),
            ));
        }
    });
    Mock {
        address,
        requests,
        connections,
    }
}

async fn read_line(reader: &mut BufReader<TcpStream>) -> Option<String> {
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
    }
}

async fn serve(
    stream: TcpStream,
    connection: usize,
    handler: Handler,
    log: Arc<Mutex<Vec<Recorded>>>,
) {
    let mut reader = BufReader::new(stream);
    loop {
        let Some(line) = read_line(&mut reader).await else {
            return;
        };
        let mut parts = line.split(' ');
        let mut recorded = Recorded {
            method: parts.next().unwrap_or_default().to_string(),
            target: parts.next().unwrap_or_default().to_string(),
            connection,
            ..Recorded::default()
        };
        loop {
            let Some(line) = read_line(&mut reader).await else {
                return;
            };
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                recorded
                    .headers
                    .push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        if recorded.header("expect") == Some("100-continue") {
            recorded.early = true;
            let reply = handler(&recorded);
            if !reply.is_empty() {
                let _ = reader.get_mut().write_all(&reply).await;
                return;
            }
            recorded.early = false;
            let _ = reader
                .get_mut()
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await;
        }
        if recorded.header("transfer-encoding") == Some("chunked") {
            loop {
                let Some(size) = read_line(&mut reader).await else {
                    return;
                };
                let size = usize::from_str_radix(&size, 16).expect("chunk size");
                let mut chunk = vec![0; size + 2];
                if reader.read_exact(&mut chunk).await.is_err() {
                    return;
                }
                if size == 0 {
                    break;
                }
                recorded.body.extend_from_slice(&chunk[..size]);
            }
        } else if let Some(length) = recorded.header("content-length") {
            let mut body = vec![0; length.parse().expect("length")];
            if reader.read_exact(&mut body).await.is_err() {
                return;
            }
            recorded.body = body;
        }
        log.lock().expect("poisoned").push(recorded.clone());
        let reply = handler(&recorded);
        if reader.get_mut().write_all(&reply).await.is_err() {
            return;
        }
        let text = String::from_utf8_lossy(&reply);
        if text.contains("Connection: close") || text.contains("X-Mock-Drop") {
            return;
        }
    }
}

// Responses

fn reply(status: &str, headers: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn multistatus(entries: &[(&str, bool, u64)]) -> Vec<u8> {
    let mut body = String::from("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">");
    for (href, collection, size) in entries {
        let kind = if *collection {
            "<d:resourcetype><d:collection/></d:resourcetype>".to_string()
        } else {
            format!("<d:resourcetype/><d:getcontentlength>{size}</d:getcontentlength>")
        };
        body.push_str(&format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{kind}\
             <d:getlastmodified>Sun, 15 Mar 2026 13:00:00 GMT</d:getlastmodified>\
             </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        ));
    }
    body.push_str("</d:multistatus>");
    reply(
        "207 Multi-Status",
        "Content-Type: application/xml\r\n",
        &body,
    )
}

fn root(base: &str) -> Vec<u8> {
    multistatus(&[(base, true, 0)])
}

async fn connect_to(mock: &Mock, base: &str) -> Result<WebDavFileSystem> {
    connect_with(mock, base, test_password()).await
}

async fn connect_with(mock: &Mock, base: &str, password: String) -> Result<WebDavFileSystem> {
    let params = ConnectionParams {
        protocol: Protocol::WebDav,
        host: "127.0.0.1".to_string(),
        port: mock.address.port(),
        username: "demo".to_string(),
        authentication: Authentication::Password(password),
    };
    let options = ConnectionOptions {
        base_path: base.to_string(),
        ..ConnectionOptions::default()
    };
    let store = std::env::temp_dir().join("hoplix-webdav-unused-certificates");
    WebDavFileSystem::connect(&params, None, &options, &store, None).await
}

// Paths

#[test]
fn base_paths_are_normalized_and_paths_encoded() {
    assert_eq!(normalize_base(""), "/");
    assert_eq!(normalize_base(" / "), "/");
    assert_eq!(
        normalize_base("remote.php/dav/files/demo"),
        "/remote.php/dav/files/demo/"
    );
    assert_eq!(normalize_base("/a%20b//c/./"), "/a b/c/");
    assert_eq!(
        encode_path("/Grüße/a b+c&d#e%f?.txt"),
        "/Gr%C3%BC%C3%9Fe/a%20b%2Bc%26d%23e%25f%3F.txt"
    );
    assert_eq!(decode_path("/a%20b/%c3%bc%zz"), "/a b/ü%zz");
}

#[tokio::test]
async fn hrefs_map_to_paths_below_the_base() {
    let server = mock(|_| root("/dav/files/demo/")).await;
    let file_system = connect_to(&server, "/dav/files/demo/")
        .await
        .expect("connected");
    let origin = format!("http://127.0.0.1:{}", server.address.port());
    assert_eq!(
        file_system.relative("/dav/files/demo/").as_deref(),
        Some("/")
    );
    assert_eq!(
        file_system.relative("/dav/files/demo/a%20b/").as_deref(),
        Some("/a b")
    );
    assert_eq!(
        file_system
            .relative(&format!("{origin}/dav/files/demo/x%23y"))
            .as_deref(),
        Some("/x#y")
    );
    assert_eq!(file_system.relative("/dav/files/demonstration"), None);
    assert_eq!(file_system.relative("/elsewhere/x"), None);
    assert_eq!(file_system.relative("relative"), None);
    assert_eq!(file_system.target("/a b/"), "/dav/files/demo/a%20b");
    assert_eq!(file_system.collection_target("/"), "/dav/files/demo/");
    assert_eq!(
        file_system.url("/a b"),
        Some(format!(
            "dav://demo@127.0.0.1:{}/dav/files/demo/a%20b",
            server.address.port()
        ))
    );
    assert_eq!(file_system.home().await, Ok("/".to_string()));
    assert!(!file_system.can_append());
    assert!(!format!("{file_system:?}").contains("Password"));
}

// Authentication and listings

#[tokio::test]
async fn digest_sign_in_and_listings_share_one_connection() {
    let server = mock(|request| {
        let Some(authorization) = request.header("authorization") else {
            return reply(
                "401 Unauthorized",
                "WWW-Authenticate: Basic realm=\"demo\"\r\n\
                 WWW-Authenticate: Digest realm=\"demo\", nonce=\"n1\", qop=\"auth\", algorithm=MD5\r\n",
                "",
            );
        };
        if !authorization.starts_with("Digest username=\"demo\"") {
            return reply("400 Bad Request", "", "");
        }
        match request.header("depth") {
            Some("0") => root("/dav/files/demo/"),
            _ => multistatus(&[
                ("/dav/files/demo/", true, 0),
                ("/dav/files/demo/a%20b%20%26%20c%23.txt", false, 1234),
                ("http://127.0.0.1/dav/files/demo/Sub/", true, 0),
                ("/dav/files/demo/Sub/deeper.txt", false, 1),
                ("/other/x", false, 1),
            ]),
        }
    })
    .await;
    let file_system = connect_to(&server, "dav/files/demo")
        .await
        .expect("connected");
    let mut entries = file_system.list("/").await.expect("listed");
    crate::sort_entries(
        &mut entries,
        crate::SortKey::Name,
        crate::SortOrder::Ascending,
    );
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["Sub", "a b & c#.txt"]);
    assert!(entries[0].is_directory() && entries[0].size.is_none());
    assert_eq!(entries[1].size, Some(1234));
    assert!(entries[1].modified.is_some());

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].header("authorization").is_none());
    assert!(requests[1..].iter().all(|request| {
        request
            .header("authorization")
            .is_some_and(|value| value.contains("nc=0000000"))
    }));
    assert_eq!(requests[2].target, "/dav/files/demo/");
    assert_eq!(requests[2].header("depth"), Some("1"));
    assert!(String::from_utf8_lossy(&requests[2].body).contains("getcontentlength"));
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn basic_is_used_when_it_is_the_only_offer() {
    let password = test_password();
    let expected = format!(
        "Basic {}",
        crate::proxy::base64(format!("demo:{password}").as_bytes())
    );
    let server = mock(move |request| {
        if request.header("authorization") == Some(expected.as_str()) {
            root("/")
        } else {
            reply(
                "401 Unauthorized",
                "WWW-Authenticate: Basic realm=\"x\"\r\n",
                "",
            )
        }
    })
    .await;
    connect_with(&server, "", password)
        .await
        .expect("connected");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn wrong_passwords_are_reported() {
    let server = mock(|_| {
        reply(
            "401 Unauthorized",
            "WWW-Authenticate: Digest realm=\"x\", nonce=\"n\"\r\n",
            "",
        )
    })
    .await;
    let error = connect_to(&server, "/").await.expect_err("accepted");
    assert_eq!(error.kind(), ErrorKind::AuthenticationFailed);
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn files_are_not_folders_and_missing_paths_are_reported() {
    let server = mock(|request| match request.target.as_str() {
        "/" => root("/"),
        "/file.txt" => multistatus(&[("/file.txt", false, 3)]),
        "/file.txt/" => multistatus(&[("/file.txt", false, 3)]),
        _ => reply("404 Not Found", "", ""),
    })
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    let entry = file_system.metadata("/file.txt").await.expect("found");
    assert_eq!((entry.name.as_str(), entry.size), ("file.txt", Some(3)));
    let error = file_system.list("/file.txt").await.expect_err("listed");
    assert_eq!(error.kind(), ErrorKind::NotADirectory);
    let error = file_system.metadata("/gone").await.expect_err("found");
    assert_eq!(error.kind(), ErrorKind::NotFound);

    let error = connect_to(&server, "/file.txt")
        .await
        .expect_err("connected");
    assert_eq!(error.kind(), ErrorKind::NotADirectory);
}

// Downloads

#[tokio::test]
async fn downloads_resume_with_ranges() {
    let server = mock(|request| match (request.target.as_str(), request.header("range")) {
        ("/", _) => root("/"),
        ("/data.bin", Some("bytes=4-")) => reply(
            "206 Partial Content",
            "Content-Range: bytes 4-9/10\r\n",
            "456789",
        ),
        ("/data.bin", Some("bytes=10-")) => reply("416 Range Not Satisfiable", "", ""),
        ("/data.bin" | "/plain.bin", _) => reply("200 OK", "", "0123456789"),
        ("/chunked.bin", _) => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n".to_vec(),
        _ => reply("404 Not Found", "", ""),
    })
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    let read = |path: &'static str, offset: u64| {
        let file_system = &file_system;
        async move {
            let mut reader = file_system.open_read(path, offset).await?;
            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .await
                .map_err(|error| Error::from_io(&error, path))?;
            Ok::<_, Error>(String::from_utf8_lossy(&data).into_owned())
        }
    };
    assert_eq!(read("/data.bin", 0).await, Ok("0123456789".to_string()));
    assert_eq!(read("/data.bin", 4).await, Ok("456789".to_string()));
    assert_eq!(read("/plain.bin", 7).await, Ok("789".to_string()));
    assert_eq!(read("/data.bin", 10).await, Ok(String::new()));
    assert_eq!(read("/chunked.bin", 0).await, Ok("abcde".to_string()));
    let error = read("/missing.bin", 0).await.expect_err("found");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    let ranges: Vec<Option<String>> = server
        .requests()
        .iter()
        .filter(|request| request.method == "GET")
        .map(|request| request.header("range").map(str::to_string))
        .collect();
    assert_eq!(ranges[1].as_deref(), Some("bytes=4-"));
    assert_eq!(ranges[0], None);
    assert!(server.connections() <= 2);
}

#[tokio::test]
async fn early_closed_downloads_do_not_poison_the_pool() {
    let big = "x".repeat(2 * 1024 * 1024);
    let server = mock(move |request| match request.target.as_str() {
        "/" => root("/"),
        _ => reply("200 OK", "", &big),
    })
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    let head = crate::read_head(&file_system, "/big.bin", 10)
        .await
        .expect("read");
    assert_eq!(head.len(), 10);
    file_system.metadata("/").await.expect("still working");
}

// Uploads

#[tokio::test]
async fn uploads_stream_chunks_after_continue() {
    let server = mock(|request| match request.method.as_str() {
        "PUT" if request.early => Vec::new(),
        "PUT" => reply("201 Created", "", ""),
        _ => root("/"),
    })
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    let data: Vec<u8> = (0..300_000u32).map(|index| (index % 251) as u8).collect();
    let mut writer = file_system
        .open_write("/new file.bin", 0)
        .await
        .expect("opened");
    for part in data.chunks(70_000) {
        writer.write_all(part).await.expect("written");
    }
    writer.shutdown().await.expect("finished");
    let put = server
        .requests()
        .into_iter()
        .find(|request| request.method == "PUT")
        .expect("no upload");
    assert_eq!(put.target, "/new%20file.bin");
    assert_eq!(put.header("transfer-encoding"), Some("chunked"));
    assert_eq!(put.body, data);

    let Err(error) = file_system.open_write("/new file.bin", 5).await else {
        panic!("resumed");
    };
    assert_eq!(error.kind(), ErrorKind::Unsupported);
}

#[tokio::test]
async fn refused_and_failed_uploads_are_reported() {
    let server = mock(
        |request| match (request.method.as_str(), request.target.as_str()) {
            ("PUT", "/locked.bin") if request.early => reply("403 Forbidden", "", ""),
            ("PUT", _) if request.early => Vec::new(),
            ("PUT", _) => reply("507 Insufficient Storage", "", ""),
            _ => root("/"),
        },
    )
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    let Err(error) = file_system.open_write("/locked.bin", 0).await else {
        panic!("opened");
    };
    assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    let mut writer = file_system
        .open_write("/full.bin", 0)
        .await
        .expect("opened");
    writer.write_all(b"data").await.expect("written");
    let error = writer.shutdown().await.expect_err("finished");
    assert!(error.to_string().contains("no space left"), "{error}");
}

#[tokio::test]
async fn dropped_writers_abort_the_upload() {
    let server = mock(|request| match request.method.as_str() {
        "PUT" if request.early => Vec::new(),
        "PUT" => reply("201 Created", "", ""),
        _ => root("/"),
    })
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    let mut writer = file_system
        .open_write("/part.bin", 0)
        .await
        .expect("opened");
    writer.write_all(b"partial").await.expect("written");
    drop(writer);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !server
            .requests()
            .iter()
            .any(|request| request.method == "PUT")
    );
}

// Other methods

#[tokio::test]
async fn folders_moves_and_deletes_use_their_methods() {
    let server = mock(
        |request| match (request.method.as_str(), request.target.as_str()) {
            ("MKCOL", "/new%20dir/") => reply("201 Created", "", ""),
            ("MKCOL", _) => reply("405 Method Not Allowed", "", ""),
            ("MOVE", "/taken") => reply("412 Precondition Failed", "", ""),
            ("MOVE", _) => reply("201 Created", "", ""),
            ("DELETE", "/gone") => reply("404 Not Found", "", ""),
            ("DELETE", _) => reply("204 No Content", "", ""),
            ("PROPFIND", "/full/") => multistatus(&[("/full/", true, 0), ("/full/x", false, 1)]),
            ("PROPFIND", "/empty/") => multistatus(&[("/empty/", true, 0)]),
            _ => root("/"),
        },
    )
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    file_system
        .create_directory("/new dir")
        .await
        .expect("created");
    let error = file_system
        .create_directory("/exists")
        .await
        .expect_err("created");
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);
    file_system.rename("/a b", "/c d").await.expect("moved");
    let error = file_system.rename("/taken", "/x").await.expect_err("moved");
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);
    file_system.remove_file("/a.txt").await.expect("deleted");
    let error = file_system.remove_file("/gone").await.expect_err("deleted");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    let error = file_system
        .remove_directory("/full")
        .await
        .expect_err("deleted");
    assert_eq!(error.kind(), ErrorKind::DirectoryNotEmpty);
    file_system
        .remove_directory("/empty")
        .await
        .expect("deleted");

    let requests = server.requests();
    let moved = requests
        .iter()
        .find(|request| request.method == "MOVE")
        .expect("no move");
    assert_eq!(moved.target, "/a%20b");
    assert_eq!(
        moved.header("destination"),
        Some(format!("http://127.0.0.1:{}/c%20d", server.address.port()).as_str())
    );
    assert_eq!(moved.header("overwrite"), Some("F"));
    assert!(
        requests
            .iter()
            .any(|request| request.method == "DELETE" && request.target == "/empty/")
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.method == "DELETE" && request.target == "/full/")
    );
}

#[tokio::test]
async fn modification_times_are_set_on_owncloud_servers_only() {
    let owncloud = "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\">\
        <d:response><d:href>/remote.php/dav/files/demo/</d:href><d:propstat><d:prop>\
        <d:resourcetype><d:collection/></d:resourcetype><oc:id>42</oc:id></d:prop>\
        <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>";
    let patched = "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">\
        <d:response><d:href>/remote.php/dav/files/demo/a.txt</d:href><d:propstat><d:prop>\
        <d:lastmodified/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>";
    let server = mock(move |request| match request.method.as_str() {
        "PROPPATCH" => reply("207 Multi-Status", "", patched),
        _ => reply("207 Multi-Status", "", owncloud),
    })
    .await;
    let file_system = connect_to(&server, "/remote.php/dav/files/demo/")
        .await
        .expect("connected");
    let time = UNIX_EPOCH + Duration::from_secs(1_773_579_600);
    file_system
        .set_modified("/a.txt", time)
        .await
        .expect("touched");
    let patch = server
        .requests()
        .into_iter()
        .find(|request| request.method == "PROPPATCH")
        .expect("no patch");
    assert!(String::from_utf8_lossy(&patch.body).contains("<d:lastmodified>1773579600<"));

    let plain = mock(|_| root("/")).await;
    let file_system = connect_to(&plain, "/").await.expect("connected");
    let error = file_system
        .set_modified("/a.txt", time)
        .await
        .expect_err("touched");
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert!(
        !plain
            .requests()
            .iter()
            .any(|request| request.method == "PROPPATCH")
    );
}

// Connections

#[tokio::test]
async fn redirects_stay_on_the_same_server() {
    let server = mock(|request| match request.target.as_str() {
        "/old/" => reply("301 Moved Permanently", "Location: /new/\r\n", ""),
        "/new/" => root("/new/"),
        "/old/away" => reply(
            "302 Found",
            "Location: http://other.example.test/away\r\n",
            "",
        ),
        _ => reply("404 Not Found", "", ""),
    })
    .await;
    let file_system = connect_to(&server, "/old/").await.expect("connected");
    let error = file_system.metadata("/away").await.expect_err("followed");
    assert!(error.detail().contains("refused"), "{error}");
    let targets: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.target.clone())
        .collect();
    assert_eq!(targets, ["/old/", "/new/", "/old/away"]);
}

#[tokio::test]
async fn closed_connections_are_reopened() {
    let server = mock(|request| {
        let mut response = root("/");
        let marker = if request.connection % 2 == 0 {
            "HTTP/1.1 207 Multi-Status\r\nConnection: close\r\n"
        } else {
            "HTTP/1.1 207 Multi-Status\r\nX-Mock-Drop: 1\r\n"
        };
        let text =
            String::from_utf8_lossy(&response).replacen("HTTP/1.1 207 Multi-Status\r\n", marker, 1);
        response = text.into_bytes();
        response
    })
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    for _ in 0..4 {
        file_system.metadata("/").await.expect("answered");
    }
    assert_eq!(server.requests().len(), 5);
    assert_eq!(server.connections(), 5);
}

#[tokio::test]
async fn unlisted_server_roots_still_connect() {
    let server = mock(|request| match request.target.as_str() {
        "/" => reply("403 Forbidden", "", ""),
        "/dav/" => multistatus(&[("/dav/", true, 0)]),
        _ => reply("404 Not Found", "", ""),
    })
    .await;
    let file_system = connect_to(&server, "").await.expect("connected");
    assert_eq!(
        file_system.list("/dav").await.map(|entries| entries.len()),
        Ok(0)
    );
    let error = connect_to(&server, "/missing/")
        .await
        .expect_err("connected");
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn stale_nonces_are_renewed_before_an_upload_starts() {
    let server = mock(|request| {
        let authorization = request.header("authorization").unwrap_or_default();
        let challenge = |nonce: &str, stale: bool| {
            let stale = if stale { ", stale=true" } else { "" };
            reply(
                "401 Unauthorized",
                &format!(
                    "WWW-Authenticate: Digest realm=\"r\", nonce=\"{nonce}\", qop=\"auth\"{stale}\r\n"
                ),
                "",
            )
        };
        match request.method.as_str() {
            _ if authorization.is_empty() => challenge("n1", false),
            "PUT" if authorization.contains("nonce=\"n1\"") => challenge("n2", true),
            "PUT" if request.early => Vec::new(),
            "PUT" => reply("204 No Content", "", ""),
            _ => root("/"),
        }
    })
    .await;
    let file_system = connect_to(&server, "/").await.expect("connected");
    let mut writer = file_system.open_write("/a.txt", 0).await.expect("opened");
    writer.write_all(b"hello").await.expect("written");
    writer.shutdown().await.expect("finished");
    let put = server
        .requests()
        .into_iter()
        .find(|request| request.method == "PUT")
        .expect("no upload");
    assert!(
        put.header("authorization")
            .is_some_and(|value| value.contains("nonce=\"n2\""))
    );
    assert_eq!(put.body, b"hello");
}
