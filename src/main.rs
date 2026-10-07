mod config;
mod game;
mod host;
mod import;
mod ipc;
mod library;
mod maps;
mod steam;
mod trigger;
mod ui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::config::Config;

/// In-game map overlay for DayZ.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the overlay in the background (the default).
    Run {
        /// Open the overlay immediately (useful for testing).
        #[arg(long)]
        show: bool,
    },
    /// Show or hide the running overlay.
    Toggle,
    /// Show the running overlay.
    Show,
    /// Hide the running overlay.
    Hide,
    /// Stop the running overlay.
    Quit,
    /// List installed maps and the maps found in the game and Workshop folders.
    List,
    /// Show what the game is doing right now (map, server, mods), as the overlay sees it.
    Status,
    /// Import maps from the game and Workshop files (the overlay also does this by itself
    /// when you join a server).
    Import {
        /// World names to import, such as chernarusplus or deerisle.
        worlds: Vec<String>,
        /// Import every map found.
        #[arg(long)]
        all: bool,
    },
    /// Import a picture of a map, for terrains the game-file importer can't read.
    ImportImage {
        /// Short id, such as `namalsk`.
        id: String,
        /// The picture; it must cover the whole terrain with north up.
        picture: PathBuf,
        /// Terrain size in metres, such as 12800.
        #[arg(long)]
        world_size: f64,
        /// Display name (defaults to the id).
        #[arg(long)]
        name: Option<String>,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Cmd::Run { show: false }) {
        Cmd::Run { show } => host::run(Config::load(), show),
        Cmd::Toggle => ipc::send(ipc::Command::Toggle),
        Cmd::Show => ipc::send(ipc::Command::Show),
        Cmd::Hide => ipc::send(ipc::Command::Hide),
        Cmd::Quit => ipc::send(ipc::Command::Quit),
        Cmd::List => list(&Config::load()),
        Cmd::Status => {
            println!("{:#?}", game::snapshot());
            Ok(())
        }
        Cmd::Import { worlds, all } => import(&Config::load(), &worlds, all),
        Cmd::ImportImage {
            id,
            picture,
            world_size,
            name,
        } => {
            let pack =
                import::import_image(&id, name.as_deref().unwrap_or(&id), world_size, &picture)?;
            println!("Imported {} into {}", pack.meta.name, pack.dir.display());
            Ok(())
        }
    }
}

fn scan(config: &Config) -> import::Catalog {
    let game_dir = config.game_dir();
    if game_dir.is_none() {
        eprintln!("DayZ install not found through Steam; set game_dir in the config.");
    }
    import::catalog::scan(&import::catalog::roots(game_dir.as_deref()))
}

fn list(config: &Config) -> Result<()> {
    println!("Installed maps ({}):", maps::maps_dir().display());
    for pack in maps::load_all() {
        println!(
            "  {:<16} {:<22} {:>6.0} m",
            pack.meta.id, pack.meta.name, pack.meta.world_size
        );
    }
    println!("\nFound in the game and Workshop folders:");
    for world in scan(config).worlds {
        let size = world.world_size.map_or("?".into(), |s| format!("{s:.0} m"));
        let economy = if world.economy.is_some() {
            "loot data"
        } else {
            "no loot data"
        };
        println!(
            "  {:<16} {:<22} {:>8}  {:>3} places  {:<12}  {}",
            world.id,
            world.name,
            size,
            world.places.len(),
            economy,
            world.source_label()
        );
    }
    Ok(())
}

fn import(config: &Config, worlds: &[String], all: bool) -> Result<()> {
    let catalog = scan(config);
    // Stay with the mod an installed map came from.
    let installed_mod = |id: &str| -> Vec<String> {
        maps::load(&maps::maps_dir().join(id.to_lowercase()))
            .ok()
            .and_then(|p| p.meta.mod_id)
            .into_iter()
            .collect()
    };
    let selected: Vec<import::WorldSource> = if all {
        catalog
            .unique()
            .iter()
            .filter_map(|w| catalog.best(&w.id, &installed_mod(&w.id)))
            .collect()
    } else {
        worlds
            .iter()
            .map(|w| {
                catalog.best(w, &installed_mod(w)).with_context(|| {
                    let names: Vec<_> = catalog.unique().iter().map(|w| w.id.clone()).collect();
                    format!("no map named {w}; found: {}", names.join(", "))
                })
            })
            .collect::<Result<_>>()?
    };
    if selected.is_empty() {
        anyhow::bail!("name the maps to import, or pass --all");
    }
    for world in &selected {
        let start = std::time::Instant::now();
        if let Ok(mut pack) = maps::load(&maps::maps_dir().join(&world.id))
            && pack.meta.source == world.fingerprint()
        {
            import::refresh_pois(world, &mut pack)?;
            println!(
                "{} is up to date; rebuilt its points of interest in {:.0?}",
                world.name,
                start.elapsed()
            );
            continue;
        }
        let last = std::sync::Mutex::new(101);
        let pack = import::import_world(world, &|p| {
            let percent = p.done * 100 / p.total;
            let mut last = last.lock().unwrap();
            if percent != *last {
                *last = percent;
                eprint!("\r{}: {percent:3}%", world.name);
            }
        })?;
        eprintln!();
        println!(
            "Imported {} into {} in {:.0?}",
            world.name,
            pack.dir.display(),
            start.elapsed()
        );
    }
    Ok(())
}
