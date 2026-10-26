//! Local read-only Inspector. Uses runtime pure projections only; never opens a runtime store.
mod compat;
mod current;
mod live;
mod privacy;
mod trace;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use compat::{digest, fingerprint, now, python_json, redact, safe, utc};
use http_body_util::Full;
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use live::{Config, live_bundle};
use serde_json::Value;
use std::{convert::Infallible, path::PathBuf, sync::Arc};
use subtle::ConstantTimeEq;

const UI: &str = include_str!("../web/index.html");
const ARCHIVE: &[u8] = include_bytes!("../web/recorded-4096-failure.json");
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
type Body = Full<Bytes>;

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}
fn json_error(raw: &str, error: &serde_json::Error) -> String {
    // serde columns count bytes; Python's JSONDecodeError counts Unicode characters.
    let line_start = raw
        .split_inclusive('\n')
        .take(error.line().saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let mut at = if error.is_eof() {
        raw.len()
    } else {
        (line_start + error.column())
            .saturating_sub(1)
            .min(raw.len())
    };
    while !raw.is_char_boundary(at) {
        at -= 1;
    }
    let message = error.to_string();
    let mut quote = None;
    let mut escape = None;
    let mut escaped = false;
    let mut containers = Vec::new();
    let mut last_quote = None;
    // Locate lexical context only; serde remains the sole JSON parser.
    for (index, c) in raw.char_indices().take_while(|(index, _)| *index <= at) {
        if quote.is_some() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
                escape = Some(index);
            } else if c == '"' {
                quote = None;
            }
        } else {
            match c {
                '"' => {
                    quote = Some(index);
                    last_quote = quote;
                    escape = None;
                }
                '{' | '[' => containers.push(c),
                '}' | ']' => {
                    containers.pop();
                }
                _ => {}
            }
        }
    }
    let last = raw
        .trim_end_matches([' ', '\t', '\r', '\n'])
        .chars()
        .next_back();
    let property = "Expecting property name enclosed in double quotes";
    let mut reason = "Expecting value";
    if message.starts_with("EOF while parsing a string") {
        reason = "Unterminated string starting at";
        at = quote.or(last_quote).unwrap_or(at);
    } else if message.starts_with("invalid escape") {
        at = escape.unwrap_or(at);
        if raw.as_bytes().get(at + 1) == Some(&b'u') {
            reason = "Invalid \\uXXXX escape";
            at += 1;
        } else {
            reason = "Invalid \\escape";
        }
    } else if message.starts_with("control character") {
        reason = "Invalid control character at";
    } else if message.starts_with("trailing comma") {
        reason = if raw.as_bytes().get(at) == Some(&b'}') {
            "Illegal trailing comma before end of object"
        } else {
            "Illegal trailing comma before end of array"
        };
        at = raw[..at].rfind(',').unwrap_or(at);
    } else if message.starts_with("trailing characters") {
        reason = "Extra data";
    } else if message.starts_with("expected `:`") {
        reason = "Expecting ':' delimiter";
    } else if message.starts_with("expected `,`") {
        reason = "Expecting ',' delimiter";
    } else if message.starts_with("key must be a string") {
        reason = property;
    } else if message.starts_with("EOF while parsing an object") {
        reason = if matches!(last, Some('{') | Some(',')) {
            property
        } else if last == Some('"')
            && last_quote.is_some_and(|start| raw[..start].trim_end().ends_with(['{', ',']))
        {
            "Expecting ':' delimiter"
        } else {
            "Expecting ',' delimiter"
        };
    } else if message.starts_with("EOF while parsing a list")
        && !matches!(last, Some('[') | Some(','))
    {
        reason = "Expecting ',' delimiter";
    } else if error.is_eof() && containers.last() == Some(&'{') && last == Some(',') {
        reason = property;
    }
    let prefix = &raw[..at];
    let offset = prefix.chars().count();
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("{reason}: line {line} column {column} (char {offset})")
}
struct Server {
    config: Config,
    origin: String,
    host: String,
    token: String,
    cookie_name: String,
}
fn reply(status: u16, body: impl Into<Bytes>, content_type: &str) -> Response<Body> {
    let body = body.into();
    Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("Content-Length", body.len())
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "no-referrer")
        .header("Content-Security-Policy", CSP)
        .body(Full::new(body))
        .unwrap()
}
fn empty(status: u16) -> Response<Body> {
    reply(status, Bytes::new(), "application/json; charset=utf-8")
}
fn unsupported(method: &str, head: bool) -> Response<Body> {
    let method = method
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        <style type=\"text/css\">\n            :root {{\n                color-scheme: light dark;\n            }}\n        </style>\n        <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        <p>Error code: 501</p>\n        <p>Message: Unsupported method ('{method}').</p>\n        <p>Error code explanation: 501 - Server does not support this operation.</p>\n    </body>\n</html>\n"
    );
    Response::builder()
        .status(501)
        .header("Content-Type", "text/html;charset=utf-8")
        .header("Content-Length", body.len())
        .header("Connection", "close")
        .body(Full::new(if head {
            Bytes::new()
        } else {
            Bytes::from(body)
        }))
        .unwrap()
}
fn cookie_value(header: &str, name: &str) -> Option<String> {
    // Match SimpleCookie: retain a parsed prefix, but reject invalid parsed items.
    static COOKIE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"\A[ \t]*(?P<key>[A-Za-z0-9!#%&'~_`><@,:/$*+\-.^|)(?}{=]+?)(?:[ \t]*=[ \t]*(?P<value>"(?:[^\\"]|\\.)*"|(?:[A-Za-z0-9_]{3,6}day|[A-Za-z0-9_]{3}),[ \t][A-Za-z0-9 \t-]{9,11}[ \t][0-9:]{8}[ \t]GMT|[A-Za-z0-9!#%&'~_`><@,:/$*+\-.^|)(?}{=\[\]]*))?[ \t]*(?:;|[ \t]+|\z)"#).unwrap()
    });
    const RESERVED: &[&str] = &[
        "expires",
        "path",
        "comment",
        "domain",
        "max-age",
        "secure",
        "httponly",
        "version",
        "samesite",
        "partitioned",
    ];
    let mut remaining = header;
    let mut seen = false;
    let mut found = None;
    while !remaining.trim().is_empty() {
        let Some(token) = COOKIE.captures(remaining) else {
            break;
        };
        remaining = &remaining[token.get(0)?.end()..];
        let key = token.name("key")?.as_str();
        let value = token.name("value").map(|v| v.as_str());
        if let Some(attribute) = key.strip_prefix('$') {
            if seen && !RESERVED.contains(&attribute.to_ascii_lowercase().as_str()) {
                return None;
            }
            continue;
        }
        if RESERVED.contains(&key.to_ascii_lowercase().as_str()) {
            if !seen
                || (value.is_none()
                    && !["secure", "httponly", "partitioned"]
                        .contains(&key.to_ascii_lowercase().as_str()))
            {
                return None;
            }
            continue;
        }
        let value = value?;
        // SimpleCookie's token grammar is broader than Morsel's legal key set.
        if !key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~:".contains(&c))
        {
            return None;
        }
        seen = true;
        if key == name {
            let mut decoded = String::new();
            if let Some(quoted) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                let mut chars = quoted.chars().peekable();
                while let Some(c) = chars.next() {
                    if c != '\\' {
                        decoded.push(c);
                        continue;
                    }
                    let escaped = chars.next()?;
                    let mut ahead = chars.clone();
                    let octal = match (escaped, ahead.next(), ahead.next()) {
                        ('0'..='3', Some(b @ '0'..='7'), Some(c @ '0'..='7')) => Some(
                            ((escaped as u8 - b'0') * 64 + (b as u8 - b'0') * 8 + (c as u8 - b'0'))
                                as char,
                        ),
                        _ => None,
                    };
                    if let Some(c) = octal {
                        decoded.push(c);
                        chars.next();
                        chars.next();
                    } else {
                        decoded.push(escaped);
                    }
                }
            } else {
                decoded.push_str(value);
            }
            found = Some(decoded);
        }
    }
    found
}
fn equal(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
async fn handle(req: Request<Incoming>, server: Arc<Server>) -> Result<Response<Body>, Infallible> {
    let method = req.method().as_str();
    if ["POST", "PUT", "DELETE", "PATCH", "OPTIONS"].contains(&method) {
        let mut response = empty(405);
        response
            .headers_mut()
            .insert("Allow", "GET".parse().unwrap());
        return Ok(response);
    }
    if method != "GET" {
        return Ok(unsupported(method, method == "HEAD"));
    }
    let header = |name: &str| req.headers().get(name).and_then(|h| h.to_str().ok());
    if header("Host") != Some(&server.host)
        || req
            .headers()
            .get("Origin")
            .is_some_and(|value| value.as_bytes() != server.origin.as_bytes())
        || header("Sec-Fetch-Site") == Some("cross-site")
    {
        return Ok(empty(403));
    }
    let path = req.uri().path();
    // BaseHTTPRequestHandler collapses leading slashes before dispatch.
    let normalized_path = path
        .starts_with("//")
        .then(|| format!("/{}", path.trim_start_matches('/')));
    let path = normalized_path.as_deref().unwrap_or(path);
    if path == "/healthz" {
        return Ok(reply(
            200,
            "{\"status\":\"ok\"}",
            "application/json; charset=utf-8",
        ));
    }
    let token = url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
        .find(|(k, v)| k == "token" && !v.is_empty())
        .map(|(_, v)| v.into_owned());
    if path == "/" && token.as_deref().is_some_and(|t| equal(t, &server.token)) {
        let mut response = empty(303);
        response
            .headers_mut()
            .insert("Location", "/".parse().unwrap());
        response.headers_mut().insert(
            "Set-Cookie",
            format!(
                "{}={}; HttpOnly; SameSite=Strict; Path=/",
                server.cookie_name, server.token
            )
            .parse()
            .unwrap(),
        );
        return Ok(response);
    }
    // Python's HTTP parser decodes header bytes as Latin-1, including quoted values.
    let cookie = req
        .headers()
        .get("Cookie")
        .map(|value| {
            value
                .as_bytes()
                .iter()
                .map(|&b| char::from(b))
                .collect::<String>()
        })
        .unwrap_or_default();
    if !cookie_value(&cookie, &server.cookie_name).is_some_and(|t| equal(&t, &server.token)) {
        return Ok(reply(
            401,
            "请使用后台启动时输出的本机访问链接。",
            "text/plain; charset=utf-8",
        ));
    }
    let response = match path {
        "/" => reply(
            200,
            UI.replace("connect-src 'none'", "connect-src 'self'"),
            "text/html; charset=utf-8",
        ),
        "/api/recorded" => reply(200, ARCHIVE, "application/json; charset=utf-8"),
        "/api/live" => match tokio::task::spawn_blocking(move || live_bundle(&server.config)).await
        {
            Ok(Ok(data)) => reply(
                200,
                python_json(&data, false),
                "application/json; charset=utf-8",
            ),
            _ => reply(
                503,
                "{\"error\":\"Data source could not be read; previous page data is stale.\"}",
                "application/json; charset=utf-8",
            ),
        },
        _ => empty(404),
    };
    Ok(response)
}
fn usage() {
    println!(
        "hippocampus Memory Inspector · 本地只读服务\n\n用法: pmr-inspector [--port PORT] [--db PATH] [--owner ID] [--log PATH ...] [--trace-dir PATH]\n       pmr-inspector --dump [上述数据参数]\n       pmr-inspector --export-trace PATH\n\n默认端口 8765；仅监听 127.0.0.1。--dump/--export-trace 输出脱敏 JSON，不启动 HTTP。"
    )
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Inspector: {error}");
        std::process::exit(1)
    }
}
async fn run() -> Result<(), String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME 未设置；请提供用户环境")?);
    let data = home.join(".local/share/personal-memory-runtime");
    let log_dir = home.join("Library/Application Support/tunnel-client/logs");
    let mut config = Config {
        db: data.join("memory.sqlite3"),
        owner: "local-user".into(),
        logs: Vec::new(),
        trace_dir: None,
    };
    let mut port = 8765u16;
    let mut dump = false;
    let mut export = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                usage();
                return Ok(());
            }
            "--dump" => dump = true,
            "--port" => {
                port = args
                    .next()
                    .ok_or("--port 缺少值")?
                    .parse()
                    .map_err(|_| "端口必须在 0..65535")?
            }
            "--db" => config.db = args.next().ok_or("--db 缺少值")?.into(),
            "--owner" => config.owner = args.next().ok_or("--owner 缺少值")?,
            "--log" => config.logs.push(args.next().ok_or("--log 缺少值")?.into()),
            "--trace-dir" => {
                config.trace_dir = Some(args.next().ok_or("--trace-dir 缺少值")?.into())
            }
            "--export-trace" => {
                export = Some(PathBuf::from(args.next().ok_or("--export-trace 缺少值")?))
            }
            _ => return Err(format!("未知参数 {arg}；使用 --help 查看用法")),
        }
    }
    if config.logs.is_empty() {
        config.logs = vec![
            log_dir.join("personal-memory.log"),
            log_dir.join("launchd.stderr.log"),
        ]
    }
    if dump && export.is_some() {
        return Err("--dump 与 --export-trace 不能同时使用".into());
    }
    if let Some(path) = export {
        println!(
            "{}",
            python_json(&live::export_trace(&config, &path)?, false)
        );
        return Ok(());
    }
    if dump {
        println!("{}", python_json(&live_bundle(&config)?, false));
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let mut random = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
        .map_err(|_| "系统随机源不可用")?;
    let host = format!("127.0.0.1:{port}");
    let origin = format!("http://{host}");
    let server = Arc::new(Server {
        config,
        origin,
        host,
        token: URL_SAFE_NO_PAD.encode(random),
        cookie_name: format!("pmr_inspector_{port}"),
    });
    println!(
        "hippocampus Memory Inspector: {}/?token={}",
        server.origin, server.token
    );
    println!("Read-only; refresh 2s; Ctrl-C to stop. No model calls.");
    // Each connection is independent; blocking snapshot I/O stays off the HTTP executor.
    loop {
        tokio::select! {
            connection=listener.accept()=>{
                let (stream,_)=connection.map_err(|e|e.to_string())?;let server=server.clone();
                tokio::spawn(async move{let _=hyper::server::conn::http1::Builder::new().title_case_headers(true).serve_connection(TokioIo::new(stream),service_fn(move|request|handle(request,server.clone()))).await;});
            }
            _=tokio::signal::ctrl_c()=>break,
        }
    }
    Ok(())
}
