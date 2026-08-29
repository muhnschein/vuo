//! Make exactly the request "Test connection" makes, and say what happened.
//!
//! For telling an app fault apart from a network one when a device reports a
//! connection failure. Run it under qemu against the target sysroot and it
//! exercises the same `Transport` the phone does -- including, usefully, the
//! fact that Miniflux authenticates with the `X-Auth-Token` header and no
//! username is sent at all.
//!
//! Usage: net-probe <server-url> [api-key]

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(url) = args.next() else {
        eprintln!("usage: net-probe <server-url> [api-key]");
        std::process::exit(2);
    };
    let token = args.next().unwrap_or_else(|| "unset".to_owned());

    let Ok(server) = url::Url::parse(&url) else {
        println!("not a URL: {url}");
        std::process::exit(1);
    };

    let config = vuo_core::api::TransportConfig::default();
    let transport = match vuo_core::api::Transport::new(
        server,
        vuo_core::redact::ApiToken::new(token),
        &config,
    ) {
        Ok(t) => t,
        Err(e) => {
            println!("could not build a client: {e}");
            std::process::exit(1);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            println!("could not start a runtime: {e}");
            std::process::exit(1);
        }
    };

    let client = vuo_core::api::MinifluxClient::new(transport);
    match runtime.block_on(client.me()) {
        Ok(user) => println!("OK: connected as {}", user.username),
        Err(e) => {
            println!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}
