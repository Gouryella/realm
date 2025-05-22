use std::env;
use cfg_if::cfg_if;
use dotenvy;

// mod api; // Removed this line
use realm::cmd;
use realm::conf::{Config, FullConf, LogConf, DnsConf, EndpointInfo};
use realm::ENV_CONFIG;

cfg_if! {
    if #[cfg(feature = "mi-malloc")] {
        use mimalloc::MiMalloc;
        #[global_allocator]
        static GLOBAL: MiMalloc = MiMalloc;
    } else if #[cfg(all(feature = "jemalloc", not(target_env = "msvc")))] {
        use jemallocator::Jemalloc;
        #[global_allocator]
        static GLOBAL: Jemalloc = Jemalloc;
    } else if #[cfg(all(feature = "page-alloc", unix))] {
        use mmap_allocator::MmapAllocator;
        #[global_allocator]
        static GLOBAL: MmapAllocator = MmapAllocator::new();
    }
}

fn main() {
    let conf = 'blk: {
        if let Ok(conf_str) = env::var(ENV_CONFIG) {
            if let Ok(conf) = FullConf::from_conf_str(&conf_str) {
                break 'blk conf;
            }
        };

        use cmd::CmdInput;
        match cmd::scan() {
            CmdInput::Endpoint(ep, opts) => {
                let mut conf = FullConf::default();
                conf.add_endpoint(ep).apply_global_opts().apply_cmd_opts(opts);
                conf
            }
            CmdInput::Config(conf, opts) => {
                let mut conf = FullConf::from_conf_file(&conf);
                conf.apply_global_opts().apply_cmd_opts(opts);
                conf
            }
            CmdInput::None => {
                // Load .env file if it exists
                if dotenvy::dotenv().is_err() {
                    log::warn!("Warning: .env file not found. API server will use default configuration or environment variables set directly.");
                }

                // Check if API_HOST or API_PORT are set, otherwise prompt for .env
                let api_host_set = env::var("API_HOST").is_ok();
                let api_port_set = env::var("API_PORT").is_ok();

                if !api_host_set && !api_port_set {
                    println!("No command line arguments or environment variables provided. To start the API server, consider creating a .env file with API_HOST and API_PORT, or set them directly in your environment.");
                    println!("Example .env content:");
                    println!("API_HOST=127.0.0.1");
                    println!("API_PORT=8080");
                    println!("API_AUTH_TOKEN=your_secret_token_here");
                }
                FullConf::default() // Create a default configuration to start the API server
            }
        }
    };

    start_from_conf(conf);
}

fn start_from_conf(full: FullConf) {
    let FullConf {
        log: log_conf,
        dns: dns_conf,
        endpoints: endpoints_conf,
        ..
    } = full;

    setup_log(log_conf);
    setup_dns(dns_conf);

    let endpoints: Vec<EndpointInfo> = endpoints_conf
        .into_iter()
        .map(Config::build)
        .inspect(|x| println!("inited: {}", x.endpoint))
        .collect();

    execute(endpoints);
}

fn setup_log(log: LogConf) {
    println!("log: {}", &log);

    let (level, output) = log.build();
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}[{}][{}]{}",
                chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"),
                record.target(),
                record.level(),
                message
            ))
        })
        .level(level)
        .chain(output)
        .apply()
        .unwrap_or_else(|e| panic!("failed to setup logger: {}", &e))
}

fn setup_dns(dns: DnsConf) {
    println!("dns: {}", &dns);

    let (conf, opts) = dns.build();
    realm::core::dns::build_lazy(conf, opts);
}

fn execute(eps: Vec<EndpointInfo>) {
    #[cfg(feature = "multi-thread")]
    {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(eps))
    }

    #[cfg(not(feature = "multi-thread"))]
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run(eps))
    }
}

async fn run(endpoints: Vec<EndpointInfo>) {
    use realm::core::tcp::run_tcp;
    use realm::core::udp::run_udp;
    use realm_core::monitor::periodically_calculate_speeds;
    use futures::future::join_all;
    use actix_web::{App, HttpServer};
    // Unused imports for specific handlers are removed as they are called with fully qualified paths.
    // The .env file is now loaded earlier in `main` function if CmdInput::None.
    // If other CmdInput variants are used, .env loading is not strictly necessary
    // as configuration is explicitly provided.

    tokio::spawn(periodically_calculate_speeds());

    // --- API Server Configuration ---
    // The API server address and port can be configured via environment variables.
    // These can be set directly in the environment or loaded from a `.env` file
    // placed in the root of the project.
    //
    // Example `.env` file content:
    // API_HOST=0.0.0.0
    // API_PORT=8081
    //
    // If `API_HOST` is not set, it defaults to "127.0.0.1".
    // If `API_PORT` is not set or is invalid, it defaults to 8080.
    // API_AUTH_TOKEN=your_secret_token_here  # Secret token for API authentication (Bearer token)
    //
    // If `API_AUTH_TOKEN` is not set, the API will be accessible without authentication (INSECURE).
    // --- End API Server Configuration ---
    let api_host = env::var("API_HOST").unwrap_or_else(|_| {
        log::debug!("API_HOST not set, defaulting to 127.0.0.1");
        "127.0.0.1".to_string()
    });

    let api_port_str = env::var("API_PORT").unwrap_or_else(|_| {
        log::debug!("API_PORT not set, defaulting to 8080");
        "8080".to_string()
    });

    let api_port = api_port_str.parse::<u16>().unwrap_or_else(|e| {
        log::warn!(
            "API_PORT value '{}' is invalid: {}. Defaulting to 8080.",
            api_port_str,
            e
        );
        8080
    });

    let expected_api_token = std::env::var("API_AUTH_TOKEN").ok().filter(|s| !s.is_empty());
    if expected_api_token.is_none() {
        log::warn!("API_AUTH_TOKEN is not set or is empty. The API will be accessible without authentication. This is strongly discouraged for production environments.");
    }

    // Clone api_host for use in the closure, as it will be moved into bind.
    let api_host_clone_for_closure = api_host.clone();
    let server = HttpServer::new(move || {
        // expected_api_token is moved into the closure
        App::new()
            .wrap(realm_core::api::RequestLogger) // Add the RequestLogger middleware
            .wrap(realm_core::api::Authenticate::new(expected_api_token.clone())) // Apply authentication middleware
            .service(realm_core::api::add_rule) // Add the new rule endpoint
            .service(realm_core::api::delete_rule) // Add the delete rule endpoint
            .service(realm_core::api::list_tcp_connections)
            .service(realm_core::api::get_tcp_connection_stats)
            .service(realm_core::api::list_udp_associations)
            .service(realm_core::api::get_udp_association_stats)
    })
    .bind((api_host_clone_for_closure, api_port))
    .unwrap_or_else(|e| panic!("Failed to bind API server to {}:{}: {}", api_host, api_port, e)) // Original api_host used in panic for clarity
    .run();

    tokio::spawn(server);
    // Use the original api_host for the log message, not the cloned one.
    log::info!("API server started at http://{}:{}", api_host, api_port);

    // If no endpoints are provided via command line or config file,
    // and we are in API mode, we should not proceed to run workers for static endpoints.
    // The API server will handle dynamic endpoint creation.
    if endpoints.is_empty() {
        log::info!("No static endpoints configured. API server is running and ready to accept dynamic rule additions.");
        // Keep the main thread alive to allow the API server to run
        // A simple way to do this is to just wait indefinitely or until a signal.
        // For now, we'll just let the runtime manage it.
        // In a real application, you might want to listen for termination signals.
        return;
    }

    let mut workers = Vec::with_capacity(2 * endpoints.len());

    for EndpointInfo {
        endpoint,
        no_tcp,
        use_udp,
    } in endpoints
    {
        if use_udp {
            workers.push(tokio::spawn(run_udp(endpoint.clone())));
        }

        if !no_tcp {
            workers.push(tokio::spawn(run_tcp(endpoint)));
        }
    }

    workers.shrink_to_fit();

    join_all(workers).await;
}
