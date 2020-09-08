#![deny(warnings)]
#![allow(unused_braces)] // bug in rustdoc: https://github.com/rust-lang/rust/issues/70814
#![recursion_limit = "1024"]

#[macro_use]
extern crate log;
#[macro_use]
extern crate pin_utils;

use anyhow::{Context, Result};
use pretty_env_logger;
use std::str::FromStr;

mod deck;
mod files;
mod importer;
mod migrations;
mod notify;
mod scryfall;
mod secrets;
mod test_helpers;
mod tts;
mod user;
mod utils;
mod web;

fn setup_logging() -> env_logger::Logger {
    let mut builder = pretty_env_logger::formatted_timed_builder();
    if let Ok(s) = std::env::var("RUST_LOG") {
        builder.parse_filters(&s);
    }

    let logger = builder.build();

    logger
}

fn setup_sentry(logger: env_logger::Logger, dsn: Option<&str>) -> Option<sentry::ClientInitGuard> {
    let opts: sentry::ClientOptions = match dsn {
        None => {
            log::set_boxed_logger(Box::new(logger)).unwrap();
            log::set_max_level(log::LevelFilter::Trace);
            return None;
        }
        Some(dsn) => dsn.into(),
    };
    let log_integration =
        sentry::integrations::log::LogIntegration::default().with_env_logger_dest(Some(logger));
    let opts = opts.add_integration(log_integration);
    Some(sentry::init(opts))
}

#[async_std::main]
async fn main() -> Result<()> {
    if let Err(e) = dotenv::dotenv() {
        eprintln!("Failed to get .env file: {}", e);
    };

    let logger = setup_logging();

    let current_dir = std::env::current_dir()?;
    let args = get_args(&current_dir);

    let mut _sentry_guard = setup_sentry(logger, args.value_of("sentry_dsn"));

    let scryfall_api = std::sync::Arc::new(scryfall::api::ScryfallApi::new());
    let mut db_pool = sqlx::PgPool::new(&format!(
        "postgresql://{}:{}@{}:{}/{}",
        args.value_of("db_user").expect("DB_USER is missing"),
        args.value_of("db_pass").expect("DB_PASSWORD is missing"),
        args.value_of("db_host").expect("DB_HOST is missing"),
        args.value_of("db_port").expect("DB_PORT is missing"),
        args.value_of("db_name").expect("DB_NAME is missing"),
    ))
    .await?;

    let redis = redis::Client::open(format!(
        "redis://{}:{}@{}:{}/",
        args.value_of("redis_user").unwrap_or(""),
        args.value_of("redis_pass").unwrap_or(""),
        args.value_of("redis_host").expect("REDIS_HOST is missing"),
        args.value_of("redis_port").expect("REDIS_port is missing"),
    ))?;
    let mut redis_conn = redis.get_async_connection().await?;

    let root = async_std::fs::canonicalize(std::path::PathBuf::from(
        args.value_of("root_folder").unwrap(),
    ))
    .await?;

    let secrets_toml_filename = {
        // .unwrap() is fine because we specify a default for this argument
        let raw: &str = args.value_of("secrets_toml").unwrap();
        let path = std::path::Path::new(raw);
        if path.is_relative() {
            let combined = root.join(&path);
            async_std::fs::canonicalize(&combined)
                .await
                .with_context(|| {
                    format!("Failed to canonicalize {:?}", combined.to_string_lossy())
                })?
        } else {
            async_std::fs::canonicalize(&path)
                .await
                .with_context(|| format!("Failed to canonicalize {:?}", path.to_string_lossy()))?
        }
    };
    crate::secrets::init_from_toml(&secrets_toml_filename).with_context(|| {
        format!(
            "Failed to initialize secrets from {:?}",
            secrets_toml_filename.to_string_lossy()
        )
    })?;

    match args.subcommand() {
        ("server", Some(server_args)) => {
            let host = std::net::IpAddr::from_str(&server_args.value_of("host").unwrap())
                .expect("host argument is not a valid IP address");
            let web_port = u16::from_str(&server_args.value_of("web_port").unwrap())
                .expect("Port argument is invalid");
            let ws_port = u16::from_str(&server_args.value_of("ws_port").unwrap())
                .expect("Port argument is invalid");
            web::run_server(scryfall_api, db_pool, redis, root, host, web_port, ws_port).await?;
        }
        ("get-card", Some(get_card_opts)) => {
            let id = scryfall::ScryfallId::from_str(get_card_opts.value_of("id").unwrap())?;
            let mut tx = db_pool.begin().await?;
            let card = scryfall::card_by_id(&mut tx, id).await?;
            tx.commit().await?;
            println!("Loaded card: {:#?}", card);
        }
        ("load-deck", Some(load_deck_opts)) => {
            // TODO: get the user from args
            let user = user::User::get_or_create_demo_user(&mut db_pool).await?;
            let url_str = load_deck_opts.value_of("url").unwrap();
            let url = url::Url::parse(&url_str)?;
            let deck = deck::load_deck(&mut db_pool, &mut redis_conn, &user, url).await?;
            println!("Loaded deck: {:#?}", deck);
        }
        ("render-deck", Some(render_deck_opts)) => {
            render_deck_command(scryfall_api, db_pool, &mut redis_conn, render_deck_opts)
                .await
                .context("Rendering a deck")?;
        }
        ("import-old", Some(opts)) => {
            let user_id = match opts.value_of("user_id") {
                Some(raw) => Some(ttsmagic_types::UserId::from_str(raw)?),
                None => None,
            };
            importer::import_all(scryfall_api, &mut db_pool, &mut redis_conn, user_id).await?;
        }
        ("cleanup", _) => {
            importer::cleanup(&mut db_pool).await?;
        }
        ("upload-files", Some(opts)) => {
            let delete_after_upload = opts.is_present("delete_after_upload");
            files::upload_all(root, delete_after_upload).await?;
        }
        ("load-scryfall-bulk", Some(load_opts)) => {
            let force = load_opts.is_present("force");
            let mut conn = db_pool.acquire().await?;
            scryfall::load_bulk(&scryfall_api, &mut conn, &root, force).await?;
        }
        ("migrate", Some(_migration_options)) => {
            migrations::apply_all(&mut db_pool).await?;
        }
        ("", None) => println!("Missing subcommand"),
        _ => unreachable!(),
    };

    Ok(())
}

async fn render_deck_command(
    scryfall_api: std::sync::Arc<scryfall::api::ScryfallApi>,
    mut db_pool: sqlx::PgPool,
    redis: &mut redis::aio::Connection,
    opts: &clap::ArgMatches<'_>,
) -> Result<()> {
    let user = user::User::get_or_create_demo_user(&mut db_pool)
        .await
        .context("Creating demo user")?;
    let url_str = opts.value_of("url").unwrap();
    let url = url::Url::parse(&url_str)?;
    let mut deck;
    {
        let mut tx = db_pool.begin().await?;
        deck = deck::load_deck(&mut tx, redis, &user, url)
            .await
            .context("Loading deck")?;
        tx.commit().await?;
    }
    let rendered;
    {
        let mut tx = db_pool.begin().await?;
        rendered = deck
            .render(scryfall_api, &mut tx, redis)
            .await
            .context("Rendering deck")?;
        tx.commit().await?;
    }

    info!("Rendered deck \"{}\" from {}", deck.title, deck.url);
    for (i, page) in rendered.pages.iter().enumerate() {
        info!("Page {}: {}", i, page.image.path());
    }
    async_std::task::spawn_blocking({
        let json: serde_json::Value = rendered.json_description.clone();
        let output_file_path: String = opts.value_of("output_file").unwrap().to_owned();
        let pretty_output: bool = opts.is_present("pretty_output");
        move || -> Result<()> {
            let (mut stdout, mut output_file);
            let output: &mut dyn std::io::Write = match output_file_path.as_str() {
                "-" => {
                    stdout = std::io::stdout();
                    &mut stdout
                }
                path => {
                    output_file = std::fs::File::create(path)?;
                    &mut output_file
                }
            };

            info!("Writing JSON to {}", output_file_path);

            if pretty_output {
                serde_json::to_writer_pretty(output, &json)?;
            } else {
                serde_json::to_writer(output, &json)?;
            }
            Ok(())
        }
    })
    .await?;

    Ok(())
}

fn get_args<'a>(current_dir: &'a std::path::Path) -> clap::ArgMatches<'a> {
    use clap::{App, Arg, SubCommand};
    App::new("ttsmagic")
        .author("Cassie Meharry <cassie@prophetessof.tech>")
        .version(clap::crate_version!())
        .about("Converts Magic: the Gathering deck lists into Tabletop Simulator decks")
        .arg(
            Arg::with_name("db_host")
                .long("db-host")
                .takes_value(true)
                .value_name("HOST")
                .default_value("localhost")
                .env("DB_HOST")
                .help("Hostname/IP of the database"),
        )
        .arg(
            Arg::with_name("db_port")
                .long("db-port")
                .takes_value(true)
                .value_name("PORT")
                .default_value("5432")
                .env("DB_PORT")
                .help("Port the database is listening on"),
        )
        .arg(
            Arg::with_name("db_name")
                .long("db-name")
                .takes_value(true)
                .value_name("NAME")
                .default_value("ttsmagic")
                .env("DB_NAME")
                .help("Database schema"),
        )
        .arg(
            Arg::with_name("db_user")
                .long("db-user")
                .takes_value(true)
                .value_name("USER")
                .default_value("ttsmagic")
                .env("DB_USER")
                .help("User to connect to the database"),
        )
        .arg(
            Arg::with_name("db_pass")
                .long("db-password")
                .takes_value(true)
                .value_name("PASS")
                .env("DB_PASSWORD")
                .help("Password to authenticate to the database"),
        )
        .arg(
            Arg::with_name("redis_host")
                .long("redis-host")
                .takes_value(true)
                .value_name("HOST")
                .env("REDIS_HOST")
                .help("Hostname/IP of the Redis server"),
        )
        .arg(
            Arg::with_name("redis_port")
                .long("redis-port")
                .takes_value(true)
                .value_name("PORT")
                .default_value("6379")
                .env("REDIS_PORT")
                .help("Port the Redis server is listening on"),
        )
        .arg(
            Arg::with_name("redis_user")
                .long("redis-user")
                .takes_value(true)
                .value_name("USER")
                .env("REDIS_USER")
                .required(false)
                .help("User to connect to the database"),
        )
        .arg(
            Arg::with_name("redis_pass")
                .long("redis-password")
                .takes_value(true)
                .value_name("PASS")
                .env("REDIS_PASSWORD")
                .required(false)
                .help("Password to authenticate to Redis"),
        )
        .arg(
            Arg::with_name("root_folder")
                .long("root-folder")
                .takes_value(true)
                .value_name("DIR")
                .default_value(current_dir.to_str().unwrap())
                .help("Root folder for runtime information"),
        )
        .arg(
            Arg::with_name("secrets_toml")
                .long("secrets-toml")
                .takes_value(true)
                .value_name("FILE.toml")
                .default_value("secrets.toml")
                .env("SECRETS_TOML")
                .help("Load secret keys from this TOML file"),
        )
        .arg(
            Arg::with_name("sentry_dsn")
                .long("sentry-dsn")
                .takes_value(true)
                .value_name("DSN")
                .env("SENTRY_DSN")
                .required(false)
                .help("Sentry DSN for error reporting"),
        )
        .subcommand(
            SubCommand::with_name("server")
                .about("Run the HTTP server")
                .arg(
                    Arg::with_name("host")
                        .long("host")
                        .takes_value(true)
                        .value_name("HOST")
                        .default_value("127.0.0.1")
                        .env("HOST")
                        .help("IP address to listen on"),
                )
                .arg(
                    Arg::with_name("web_port")
                        .long("web-port")
                        .takes_value(true)
                        .value_name("PORT")
                        .default_value("8080")
                        .env("WEB_PORT")
                        .help("Port to listen on (web HTTP)"),
                )
                .arg(
                    Arg::with_name("ws_port")
                        .long("ws-port")
                        .takes_value(true)
                        .value_name("PORT")
                        .default_value("8081")
                        .env("WS_PORT")
                        .help("Port to listen on (websocket)"),
                ),
        )
        .subcommand(
            SubCommand::with_name("get-card")
                .about("Fetch a card by ID from Scryfall")
                .arg(
                    Arg::with_name("id")
                        .takes_value(true)
                        .value_name("ID")
                        .help("Scryfall ID (UUID)"),
                ),
        )
        .subcommand(
            SubCommand::with_name("load-deck")
                .about("Parse a deck from a URL")
                .arg(
                    Arg::with_name("url")
                        .takes_value(true)
                        .value_name("URL")
                        .help("URL of a page listing the deck"),
                ),
        )
        .subcommand(
            SubCommand::with_name("render-deck")
                .about("Render a deck to images and JSON")
                .arg(
                    Arg::with_name("url")
                        .takes_value(true)
                        .value_name("URL")
                        .help("URL of a page listing the deck"),
                )
                .arg(
                    Arg::with_name("output_file")
                        .takes_value(true)
                        .value_name("FILE")
                        .short("o")
                        .long("output")
                        .default_value("-")
                        .help("Filename to write JSON output to"),
                )
                .arg(
                    Arg::with_name("pretty_output")
                        .short("p")
                        .long("pretty")
                        .takes_value(false)
                        .help("Pretty-print the JSON output"),
                ),
        )
        .subcommand(
            SubCommand::with_name("import-old")
                .about("Import users and decks from the old Python version of ttsmagic.cards")
                .arg(
                    Arg::with_name("user_id")
                        .long("user-id")
                        .takes_value(true)
                        .help("Only import decks for this user ID"),
                ),
        )
        .subcommand(SubCommand::with_name("cleanup").about("Cleanup URLs and duplication issues"))
        .subcommand(
            SubCommand::with_name("upload-files")
                .about("Upload local files to cloud storage")
                .arg(
                    Arg::with_name("delete_after_upload")
                        .long("delete")
                        .takes_value(false)
                        .help("Deletes the local files after uploading"),
                ),
        )
        .subcommand(
            SubCommand::with_name("load-scryfall-bulk")
                .about("Load card list from Scryfall")
                .arg(
                    Arg::with_name("force")
                        .long("force")
                        .help("Ignore cache and always re-download from Scryfall"),
                ),
        )
        .subcommand(SubCommand::with_name("migrate").about("Run database migrations"))
        .get_matches()
}
