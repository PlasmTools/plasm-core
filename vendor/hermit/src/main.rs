// not unit-testable, the other unit tests and the API tests do a decent job overall though

#![forbid(unsafe_code)]
use beavuck_hermit::resource_generator::set_use_examples;
use beavuck_hermit::{cli, constants, router, spec_loader};
use clap::Parser;

#[cfg_attr(test, mutants::skip)] // not tested
#[tokio::main]
async fn main() {
    let args = cli::Args::parse();
    args.validate().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    set_use_examples(args.use_examples);
    let routes = match args.specs_dir {
        Some(ref dir) => spec_loader::load_dir(dir),
        None => spec_loader::load_all(&args.specs),
    };
    let app = router::with_cors(
        router::build_with_bounds(routes, args.min_items, args.max_items),
        &args.cors_allowed_origins,
    )
    .layer(axum::middleware::from_fn(log_request));

    let addr = (constants::BIND_ADDR, args.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("listener should bind to the configured address");
    print_banner(args.port);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server should serve without error");
}

#[cfg_attr(test, mutants::skip)] // not tested
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler should register");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("ctrl_c handler should register");
}

#[cfg_attr(test, mutants::skip)] // not tested
async fn log_request(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let now = chrono::Local::now();
    let response = next.run(req).await;
    println!(
        "{} {} {} --> {}",
        now.format("%Y-%m-%d %H:%M:%S%.3f"),
        method,
        path,
        response.status()
    );
    response
}

#[cfg_attr(test, mutants::skip)] // not tested
fn print_banner(port: u16) {
    let version = env!("CARGO_PKG_VERSION");
    println!(
        r#"
| |                               | |                                          ,=.
| |__   ___  __ ___   ___   _  ___| | __                        ,=""""==.__.="  o".___
| '_ \ / _ \/ _` \ \ / / | | |/ __| |/ /                  ,=.=="                  ___/
| |_) |  __/ (_| |\ V /| |_| | (__|   <             ,==.,"    ,          , \,===""
|_.__/ \___|\__,_| \_/  \__,_|\___|_|\_\          <     ,==)  \"'"=._.==)  \
                                                    `==''    `"           `"
                   __
                .-(  )-.
               (  (  )  )
              (   (  )   )
              ..//(o o)\\..                       hermit v{version}

 Ready on port {port} (if this is running in a container, this port number is internal to the container)
"#
    );
}
