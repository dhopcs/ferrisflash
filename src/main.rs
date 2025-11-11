use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use clap::Parser;

mod fs;
mod gui;

#[derive(Debug, Parser)]
#[clap(version)]
struct Args {
    #[clap(short, long)]
    verbose: bool,
    #[clap(short, long, default_value = "")]
    image_path: String,
    #[clap(short, long, default_value = "")]
    device_path: String,
    #[clap(short, long, default_value = "false")]
    gui: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.gui {
        gui::run_gui(args)?;
        return Ok(());
    }

    // Validate required arguments for CLI mode
    if args.image_path.is_empty() || args.device_path.is_empty() {
        eprintln!("Error: --image-path and --device-path are required when not using GUI mode");
        std::process::exit(1);
    }

    let progress = Arc::new(Mutex::new(fs::Progress::new(0)));
    let progress_clone = Arc::clone(&progress);

    thread::spawn(move || {
        update_progress_bar(progress_clone);
    });

    fs::flash_images(&args.image_path, vec![&args.device_path], progress.clone())?;

    println!();

    println!("Completed in {:?}", progress.lock().unwrap().get_elapsed_time());

    Ok(())
}

fn update_progress_bar(progress: Arc<Mutex<fs::Progress>>) {
    use std::io::{self, Write};
    loop {
        let progress_guard = progress.lock().unwrap();
        let percent = progress_guard.get_progress() * 100.0;
        let speed = progress_guard.get_speed_bytes() / 1_048_576.0;

        print!("\r\x1B[2K");
        print!("Progress: {:.2}% | Speed: {:.2} MB/s | Elapsed: {}s",
                percent, speed, progress_guard.get_elapsed_time().as_secs());
        io::stdout().flush().unwrap();

        drop(progress_guard);
        thread::sleep(Duration::from_millis(200));
    }
}
