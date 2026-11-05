//! 本机回环 HTTP 服务端：**一次登录一台**。
//!
//! ── 这一层存在的理由 ────────────────────────────────────────
//! 有两家网页登录必须在本机起一台小服务器：
//!   * AStudio 的登录页要能加载页面、把极验与表单结果 POST 回来；
//!   * Kiro 的 OAuth 回调落在本机端口上，其中 AWS 那一段还得**由我们 302
//!     把浏览器送去 AWS**（不是收下就了事）。
//! 它们的共同点只有三件：只绑回环、要一个端口、要能把请求变成一个响应 ——
//! 其余全是各家自己的事。
//!
//! ── 为什么收敛成 axum ───────────────────────────────────────
//! 收敛之前是两种做法并存：`trae/callback_server.rs` 用 axum，
//! `astudio/login_page.rs` 手搓了读请求 / 解析 query / 写响应（约 140 行）。
//! 手搓那份少了 `+` 解空格、chunked 请求体、keep-alive、慢请求超时 —— 这些都是
//! 「在某个浏览器上才犯」的错误，而不是本地能试出来的。所以这里**不是把那份
//! 手搓代码挪个位置**（挪位置只是搬家，仍然是手搓），而是把「怎么跑一台回环
//! 服务器」定成一种做法，两家都建在它上面。
//!
//! ── 与 `core::login_listener` 的分工 ────────────────────────
//! 那个是「Core 态占住网关端口，让 OAuth 回调有地方落」，长期存在；
//! 这里是**一次登录一台、结束即释放**的临时端口，数量不随登录次数累积。
//!
//! ── 端口为什么必须可 abort ──────────────────────────────────
//! 会话对象被 `Arc` 共享着（等待方与任务表各持一份），只靠 `Drop` 释放端口会
//! 变成「等超时才还」—— 取消一次登录后立刻重试就抢不到端口。所以
//! [`serve`] 返回的是一个**句柄**，收尾路径显式调它。

use std::net::{Ipv4Addr, Ipv6Addr};

use axum::response::Html;
use axum::Router;
use tokio::net::TcpListener;

/// 单次请求体上限（这些页面只发几 KB 的表单 / JSON）。
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// 绑一个系统分配的端口（上一次登录还没收尾时不会撞车）。
pub async fn bind_any() -> Result<(TcpListener, u16), String> {
    bind_on(0).await
}

/// 按给定顺序逐个试，绑上一个就算（上游把回调端口写死在白名单里时用）。
///
/// 报错要把「最后一个失败原因」带上：端口被占的报错原文（`address already
/// in use`）比我们自己编的"启动失败"有用得多。
pub async fn bind_from(ports: &[u16]) -> Result<(TcpListener, u16), String> {
    let mut last = String::new();
    for port in ports {
        match bind_on(*port).await {
            Ok(pair) => return Ok(pair),
            Err(error) => last = error,
        }
    }
    Err(if last.is_empty() {
        "没有可用的本机端口".to_string()
    } else {
        format!("{last}（候选端口都被占用了，关掉那个程序后重试）")
    })
}

async fn bind_on(port: u16) -> Result<(TcpListener, u16), String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|error| format!("绑定本机端口 {port} 失败：{error}"))?;
    let actual = listener
        .local_addr()
        .map_err(|error| format!("读不到本机端口号：{error}"))?
        .port();
    Ok((listener, actual))
}

/// 起服务，返回关闭句柄（`abort()` 即释放端口）。
///
/// 同一端口上还会尽力再绑一份 `::1`：授权地址里写的可能是 `localhost`，
/// 而它解析成 `127.0.0.1` 还是 `::1` 由系统的解析顺序决定 —— 只绑 IPv4 时
/// 个别环境会先连 `::1` 而连不上（`trae::callback_server` 同一条处置）。
/// 拿不到那份不影响主体，所以失败不算错。
pub async fn serve(listener: TcpListener, router: Router) -> tokio::task::AbortHandle {
    let port = listener
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or_default();
    if let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
        spawn_one(v6, router.clone());
    }
    spawn_one(listener, router)
}

fn spawn_one(listener: TcpListener, router: Router) -> tokio::task::AbortHandle {
    // 走 `crate::spawn_task` 而不是 `tokio::spawn`：本 crate 有两种宿主
    // （Tauri 运行时 / headless 自建运行时），只有这个入口在两边都不 panic。
    let handle = crate::spawn_task(async move {
        // 监听器被 abort 会让这里以 io 错误返回，那是**正常收尾**，不记日志
        let _ = axum::serve(listener, router).await;
    });
    handle.abort_handle()
}

/// 结果页（✓ / ✕），亮暗色跟随系统。
///
/// 文案里可能带上游回给我们的原文（`error_description`、AWS 的错误说明），
/// 而**那是从查询串里来的**：谁都能往本机端口上打一个带 HTML 的请求。
/// 所以这里一律转义后再拼 —— 不转义就是把回环端口变成一个 XSS 入口。
pub fn page(ok: bool, title: &str, text: &str) -> Html<String> {
    let color = if ok { "#2a9d5c" } else { "#c0392b" };
    let mark = if ok { "✓" } else { "✕" };
    Html(format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{font:15px system-ui,sans-serif;display:grid;place-items:center;\
         min-height:90vh;margin:0;color:#222;background:#fafafa}}\
         @media (prefers-color-scheme:dark){{body{{color:#eee;background:#161616}}}}\
         main{{text-align:center;max-width:28rem;padding:1rem}}\
         .d{{font-size:2rem;color:{color}}}</style>\
         <main><div class=\"d\">{mark}</div><h1>{title}</h1><p>{text}</p></main>",
        title = escape(title),
        text = escape(text),
    ))
}

/// HTML 文本转义（只做够用的五个字符，页面里没有别的插入点）。
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn the_page_escapes_upstream_text() {
        // 上游的 error_description 是查询串里来的：带尖括号必须被转义，
        // 否则本机回环端口就成了一个 XSS 入口
        let html = page(false, "登录没有完成", "<img src=x onerror=alert(1)>").0;
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains("&lt;img"), "{html}");
        assert!(html.contains("✕"), "失败页要有叉号：{html}");
        let done = page(true, "登录完成", "可以关闭这个标签页了").0;
        assert!(done.contains("✓"));
        assert_eq!(escape("a&b<c>\"d\""), "a&amp;b&lt;c&gt;&quot;d&quot;");
    }

    #[tokio::test]
    async fn binding_any_yields_a_usable_port() {
        let (listener, port) = bind_any().await.expect("系统分配端口不该失败");
        assert_ne!(port, 0);
        // 真能连上（服务还没起，connect 也可能成；这里只验端口是通的）
        let connected = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        .expect("2 秒内应连得上");
        assert!(connected.is_ok(), "绑上的端口连不上");
        drop(listener);
    }

    #[tokio::test]
    async fn a_request_reaches_the_handler_and_gets_the_reply() {
        // 这一条是共享原语的回归护栏：读请求 → 交给路由 → 写响应整条链。
        // 手搓那版曾经在这条链上出过「慢请求读死」与「响应不关闭连接」。
        let (listener, port) = bind_any().await.expect("端口可绑");
        let router = Router::new().route(
            "/ping",
            get(|| async { Html(String::from("<b>pong</b>")) }),
        );
        let handle = serve(listener, router).await;

        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("服务应已在监听");
        let request = format!("GET /ping?x=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("请求要写进去");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
            .await
            .expect("5 秒内应读完回执")
            .expect("回执可读");
        let text = String::from_utf8_lossy(&response);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("<b>pong</b>"), "{text}");

        // abort 之后端口当场还回去（取消一次登录就要这个语义）
        handle.abort();
        let mut released = false;
        for _ in 0..40 {
            if tokio::net::TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
                released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(released, "abort 之后端口没还回来（{port}）");
    }

    #[tokio::test]
    async fn the_candidate_list_reports_the_last_failure() {
        // 空列表与「全被占用」都必须给出可读报错，而不是静默返回一个 0 端口
        let error = bind_from(&[]).await.expect_err("空候选列表必须失败");
        assert!(error.contains("端口"), "{error}");
        let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("占一个端口");
        let port = occupied.local_addr().map(|addr| addr.port()).unwrap_or_default();
        let error = bind_from(&[port]).await.expect_err("占用的端口必须失败");
        assert!(error.contains("候选端口都被占用"), "{error}");
    }
}
