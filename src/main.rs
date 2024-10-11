mod app;
mod events;
mod resources;

use app::App;
use assets::assetcatalog::AssetCatalog;
use cli::{commands::Commands, ARGS};
use cliclack::{spinner, ProgressBar};
use file_system::{FileSystem, State};
use scopeguard::{defer, defer_on_unwind, guard_on_unwind};
use std::{
    process::ExitCode,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, RwLock,
    },
};
use tokio::{
    self,
    signal::ctrl_c,
    task::{self},
    time::{self, Duration, Instant},
};
use utils::{format_bytes, format_duration};

#[tokio::main]
async fn main() -> tokio::io::Result<ExitCode> {
    // console_subscriber::init();
    let app = App::init();
    // parse_lumberyard_source().await.unwrap();
    // map_resources().await;
    // return Ok(());

    let token = app.cancel.clone();
    tokio::spawn(async move {
        ctrl_c().await.unwrap();
        token.cancel();
    });

    run().await?;

    Ok(ExitCode::SUCCESS)
}

async fn run() -> tokio::io::Result<()> {
    // TODO: handle this differently

    let (cwd, out_dir, filter) = match &ARGS.command {
        Commands::Extract(extract) => (
            extract.common.input.input.as_ref().unwrap(),
            extract.common.output.output.as_ref().unwrap(),
            extract.common.filter.filter.as_ref(),
        ),
    };

    let fs = tokio::spawn(async move {
        let pb = cliclack::spinner();
        pb.start("Initializing File System");
        let fs = FileSystem::init(cwd, out_dir, App::handle().cancel.clone()).await;
        pb.stop("File System Initialized");

        // let pb = cliclack::spinner();
        // pb.start("Initializing Asset Catalog");
        // let _ = AssetCatalog::init().await.unwrap();
        // pb.stop("Asset Catalog Initialized");
        fs
    })
    .await?;

    let locale = fs.load_localization("en-us").await;
    dbg!(locale);

    todo!();

    let files = fs.files(filter);
    let len = files.len() as u64;
    // let size: usize = files.iter().map(|(_, (_, _, size))| size).sum();

    let multi_pb = Arc::new(cliclack::MultiProgress::new("Extracting Pak(s)"));
    let all = Arc::new(multi_pb.add(ProgressBar::new(len)));
    let stats_pb = Arc::new(multi_pb.add(spinner()));
    let pak_pb = Arc::new(multi_pb.add(spinner()));
    let file_pb = Arc::new(multi_pb.add(spinner()));

    // multi_pb.println("Processing");

    all.start("");
    stats_pb.start("");
    file_pb.start("");
    pak_pb.start("");

    let bytes = Arc::new(AtomicU64::new(0));
    let processed = Arc::new(AtomicU64::new(0));

    let processed_clone = processed.clone();
    let bytes_cloned = Arc::clone(&bytes);
    let start = Instant::now();
    let all_pb = Arc::clone(&all);
    let state = Arc::new(RwLock::new(State {
        active: Arc::new(AtomicUsize::new(0)),
        max: Arc::new(AtomicUsize::new(0)),
        size: Arc::new(AtomicUsize::new(0)),
    }));
    let state_clone = state.clone();
    let stats_pb_clone = stats_pb.clone();

    task::spawn(async move {
        let mut interval = time::interval(Duration::from_millis(1000 / 10));

        loop {
            if App::handle().cancel.is_cancelled() {
                break;
            };

            interval.tick().await;
            let elapsed = start.elapsed();
            // let dots = ".".repeat(((elapsed.as_secs() % 3) + 1) as usize);
            // multi_pb_clone.println(format!("Processing{}", dots));
            let bytes_prcocessed = bytes_cloned.load(Ordering::Relaxed);
            let bytes_per_sec = bytes_prcocessed as f64 / elapsed.as_secs_f64();
            let processed_count = processed_clone.load(Ordering::Relaxed);
            let eta = if processed_count < len {
                let remaining = len - processed_count;
                let time_per_file = elapsed.as_secs_f64() / (processed_count + 1) as f64;
                Duration::from_secs_f64(time_per_file * remaining as f64)
                // let remaining = size - bytes_prcocessed as u128;
                // if bytes_per_sec > 0.0 {
                //     Duration::from_secs_f64(remaining as f64 / bytes_per_sec)
                // } else {
                //     Duration::ZERO
                // }
            } else {
                Duration::ZERO
            };

            let state = state_clone.read().unwrap();

            stats_pb_clone.set_message(format!(
                "#Tasks: {} | Max Tasks: {} | #Last Bytes Written: {} ",
                state.active.load(Ordering::Relaxed),
                state.max.load(Ordering::Relaxed),
                format_bytes(state.size.load(Ordering::Relaxed) as f64),
            ));
            all_pb.set_message(format!(
                "ETA: {} | Throughput: {}/s",
                format_duration(eta),
                format_bytes(bytes_per_sec),
                // format_bytes(size as f64)
            ));
        }
    });

    let multi_pb_clone = multi_pb.clone();
    tokio::spawn(async move {
        App::handle().cancel.cancelled().await;
        multi_pb_clone.println("Aborting...");
        multi_pb_clone.cancel();
    });

    let all_pb = Arc::clone(&all);
    let cloned_processed = processed.clone();
    let bytes_cloned = Arc::clone(&bytes);
    let file_pb_clone = file_pb.clone();
    let pak_pb_clone = pak_pb.clone();

    fs.all(files, state.clone(), move |pak, entry, len, idx, size| {
        bytes_cloned.fetch_add(size, Ordering::Relaxed);
        all_pb.inc(1);
        pak_pb_clone.set_message(format!(
            "{} ({idx}/{len})",
            pak.file_name().unwrap().to_str().unwrap()
        ));

        file_pb_clone.set_message(format!("{}", entry.display()));

        let processed = Arc::clone(&cloned_processed);
        if (processed.fetch_add(1, Ordering::Relaxed) + 1) == len as u64 {};

        Ok(())
    })
    .await?;

    let all_pb = all.clone();
    let file_pb = file_pb.clone();
    let pak_pb = pak_pb.clone();
    all_pb.stop("");
    file_pb.stop("");
    pak_pb.stop("");
    stats_pb.stop("");
    multi_pb.stop();
    let processed = processed.load(Ordering::Relaxed);
    let bytes_cloned = Arc::clone(&bytes);

    cliclack::outro(format!(
        "Processed {}/{} files in {}. Bytes: {}",
        processed,
        len,
        format_duration(start.elapsed()),
        format_bytes(bytes_cloned.load(Ordering::Relaxed) as f64)
    ))
    .unwrap();

    Ok(())
}
