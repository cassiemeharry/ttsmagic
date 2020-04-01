#![deny(unused)]

#[macro_use]
extern crate log;
#[macro_use]
extern crate pin_utils;

use failure::{Error, ResultExt};
use pretty_env_logger;
use std::str::FromStr;

mod deck;
mod files;
mod migrations;
mod scryfall;
mod tts;
mod user;
mod utils;
mod web;

#[async_std::main]
async fn main() -> Result<(), Error> {
    pretty_env_logger::init();

    let current_dir = std::env::current_dir()?;
    let args = get_args(&current_dir);

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

    let root = async_std::fs::canonicalize(std::path::PathBuf::from(
        args.value_of("root_folder").unwrap(),
    ))
    .await?;

    match args.subcommand() {
        ("server", Some(server_args)) => {
            let host = std::net::IpAddr::from_str(&server_args.value_of("host").unwrap())
                .expect("host argument is not a valid IP address");
            let port = u16::from_str(&server_args.value_of("port").unwrap())
                .expect("Port argument is invalid");
            web::run_server(scryfall_api, db_pool, root, host, port).await?;
        }
        ("get-card", Some(get_card_opts)) => {
            let id = scryfall::ScryfallId::from_str(get_card_opts.value_of("id").unwrap())?;
            let mut tx = db_pool.begin().await?;
            let card = scryfall::ensure_card(&scryfall_api, &mut tx, id).await?;
            tx.commit().await?;
            println!("Loaded card: {:#?}", card);
        }
        ("load-deck", Some(load_deck_opts)) => {
            // TODO: get the user from args
            let user = user::User::get_or_create_demo_user(&mut db_pool).await?;
            let url = load_deck_opts.value_of("url").unwrap();
            // let mut tx = db_pool.begin().await?;
            let deck = deck::load_deck(&scryfall_api, &mut db_pool, user.id, url).await?;
            // tx.commit().await?;
            println!("Loaded deck: {:#?}", deck);
        }
        ("render-deck", Some(render_deck_opts)) => {
            render_deck_command(scryfall_api, db_pool, root, render_deck_opts)
                .await
                .context("Rendering a deck")?;
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
    root: async_std::path::PathBuf,
    opts: &clap::ArgMatches<'_>,
) -> Result<(), Error> {
    let user = user::User::get_or_create_demo_user(&mut db_pool)
        .await
        .context("Creating demo user")?;
    let url = opts.value_of("url").unwrap();
    let mut deck;
    {
        let mut tx = db_pool.begin().await?;
        deck = deck::load_deck(&scryfall_api, &mut tx, user.id, url)
            .await
            .context("Loading deck")?;
        tx.commit().await?;
    }
    let rendered;
    {
        let mut tx = db_pool.begin().await?;
        rendered = deck
            .render(scryfall_api, &mut tx, &root)
            .await
            .context("Rendering deck")?;
        tx.commit().await?;
    }

    info!("Rendered deck \"{}\" from {}", deck.title, deck.url);
    for (i, page) in rendered.pages.iter().enumerate() {
        info!("Page {}: {}", i, page.image.path().to_string_lossy());
    }
    async_std::task::spawn_blocking({
        let json: serde_json::Value = rendered.json_description.clone();
        let output_file_path: String = opts.value_of("output_file").unwrap().to_owned();
        let pretty_output: bool = opts.is_present("pretty_output");
        move || -> Result<(), Error> {
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
                .default_value("localhost")
                .env("DB_HOST")
                .help("Hostname/IP of the database"),
        )
        .arg(
            Arg::with_name("db_port")
                .long("db-port")
                .takes_value(true)
                .default_value("5432")
                .env("DB_PORT")
                .help("Port the database is listening on"),
        )
        .arg(
            Arg::with_name("db_name")
                .long("db-name")
                .takes_value(true)
                .default_value("ttsmagic")
                .env("DB_NAME")
                .help("Database schema"),
        )
        .arg(
            Arg::with_name("db_user")
                .long("db-user")
                .takes_value(true)
                .default_value("ttsmagic")
                .env("DB_USER")
                .help("User to connect to the database"),
        )
        .arg(
            Arg::with_name("db_pass")
                .long("db-password")
                .takes_value(true)
                .env("DB_PASSWORD")
                .help("Password to authenticate to the database"),
        )
        .arg(
            Arg::with_name("root_folder")
                .long("root-folder")
                .takes_value(true)
                .default_value(current_dir.to_str().unwrap())
                .help("Root folder for runtime information"),
        )
        .subcommand(
            SubCommand::with_name("server")
                .about("Run the HTTP server")
                .arg(
                    Arg::with_name("host")
                        .short("h")
                        .long("host")
                        .takes_value(true)
                        .default_value("127.0.0.1")
                        .env("HOST")
                        // .validator(|s| {
                        //     <std::net::IpAddr as FromStr>::from_str(&s)
                        //         .map(|_| ())
                        //         .map_err(|e| format!("{}", e))
                        // })
                        .help("IP address to listen on"),
                )
                .arg(
                    Arg::with_name("port")
                        .short("p")
                        .long("port")
                        .takes_value(true)
                        .default_value("8080")
                        .env("PORT")
                        // .validator(|s| {
                        //     <u16 as FromStr>::from_str(&s)
                        //         .map(|_| ())
                        //         .map_err(|e| format!("{}", e))
                        // })
                        .help("Port to listen on"),
                ),
        )
        .subcommand(
            SubCommand::with_name("get-card")
                .about("Fetch a card by ID from Scryfall")
                .arg(
                    Arg::with_name("id")
                        .takes_value(true)
                        .help("Scryfall ID (UUID)"),
                ),
        )
        .subcommand(
            SubCommand::with_name("load-deck")
                .about("Parse a deck from a URL")
                .arg(
                    Arg::with_name("url")
                        .takes_value(true)
                        .help("URL of a page listing the deck"),
                ),
        )
        .subcommand(
            SubCommand::with_name("render-deck")
                .about("Render a deck to images and JSON")
                .arg(
                    Arg::with_name("url")
                        .takes_value(true)
                        .help("URL of a page listing the deck"),
                )
                .arg(
                    Arg::with_name("output_file")
                        .takes_value(true)
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
