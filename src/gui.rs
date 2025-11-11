use eframe::egui;
use std::sync::{Arc, Mutex};
use std::thread;
use crate::fs::{DeviceInfo, Progress};
use crate::{Args, fs};

// Ferris SVG asset, curtosy of https://rustacean.net/
const FERRIS_SVG: &[u8] = include_bytes!("../assets/ferris.svg");

#[derive(Clone, Copy, PartialEq)]
enum FlashingState {
    Idle,
    InProgress,
    Completed,
    Error,
}

struct State {
    image_path: String,
    device_paths: Vec<String>,
    flashing_state: FlashingState,
    progress: Arc<Mutex<Progress>>,
    error_message: Option<&'static str>,
    success_message: Option<String>,
    available_devices: Vec<DeviceInfo>,
    selected_device_indices: Vec<usize>,
    refresh_devices: bool,
    completed_time: Option<u64>,
}

impl State {
    fn new(args: Args) -> Self {
        let available_devices = fs::enumerate_devices();
        let (device_paths, selected_device_indices) = if !args.device_path.is_empty() {
            if let Some(index) = available_devices.iter().position(|d| d.path == args.device_path) {
                (vec![args.device_path], vec![index])
            } else {
                (vec![args.device_path], vec![])
            }
        } else {
            (vec![], vec![])
        };

        Self {
            image_path: args.image_path,
            device_paths,
            flashing_state: FlashingState::Idle,
            progress: Arc::new(Mutex::new(Progress::new(0))),
            error_message: None,
            success_message: None,
            available_devices,
            selected_device_indices,
            refresh_devices: false,
            completed_time: None,
        }
    }

    fn start_flashing(&mut self) {
        if self.image_path.is_empty() || self.device_paths.is_empty() {
            self.error_message = Some("Please select both image and device paths");
            return;
        }

        self.flashing_state = FlashingState::InProgress;
        self.error_message = None;
        self.success_message = None;
        self.completed_time = None; // Reset completion time when starting new flash

        let image_path = self.image_path.clone();
        let device_paths = self.device_paths.clone();
        let progress = Arc::clone(&self.progress);

        thread::spawn(move || {
            // Flash to all devices simultaneously
            let result = fs::flash_images(&image_path, device_paths, progress.clone());
            if result.is_err() {
                if let Ok(mut progress_guard) = progress.lock() {
                    *progress_guard = Progress::new(0);
                }
            }
        });
    }
}

impl eframe::App for State {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check flashing progress
        if self.flashing_state == FlashingState::InProgress {
            if let Ok(progress) = self.progress.lock() {
                if progress.get_progress() >= 1.0 {
                    self.flashing_state = FlashingState::Completed;
                    let elapsed = progress.get_elapsed_time().as_secs();
                    self.completed_time = Some(elapsed); // Store the completion time
                    self.success_message = Some(format!(
                        "Flashing completed in {:.1}s!",
                        elapsed as f32
                    ));
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Refresh devices if needed
            if self.refresh_devices {
                self.available_devices = fs::enumerate_devices();
                self.refresh_devices = false;
                // Update selected device indices if the current device paths are still available
                self.selected_device_indices.clear();
                for device_path in &self.device_paths {
                    if let Some(index) = self.available_devices.iter().position(|d| &d.path == device_path) {
                        self.selected_device_indices.push(index);
                    }
                }
            }

            // Header with Ferris logo and title side by side
            ui.horizontal(|ui| {
                // Load and draw Ferris SVG
                let opt = usvg::Options::default();
                if let Ok(tree) = usvg::Tree::from_data(FERRIS_SVG, &opt) {
                    let size = tree.size();

                    // Maintain aspect ratio - scale to 50px height
                    let target_height = 50.0;
                    let aspect_ratio = size.width() / size.height();
                    let ferris_size = egui::vec2(target_height * aspect_ratio, target_height);

                    // Render SVG to image
                    let pixmap_size = resvg::tiny_skia::IntSize::from_wh(
                        ferris_size.x as u32,
                        ferris_size.y as u32,
                    ).unwrap();

                    if let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(pixmap_size.width(), pixmap_size.height()) {
                        let scale_x = ferris_size.x / size.width();
                        let scale_y = ferris_size.y / size.height();
                        let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);

                        resvg::render(&tree, transform, &mut pixmap.as_mut());

                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [pixmap.width() as _, pixmap.height() as _],
                            pixmap.data(),
                        );

                        let texture = ctx.load_texture("ferris", image, Default::default());

                        ui.add(egui::Image::new(&texture).max_size(ferris_size));
                    }
                }

                ui.vertical(|ui| {
                    ui.add_space(5.0);
                    ui.heading(egui::RichText::new("ferrisflash").size(28.0));
                    ui.label(egui::RichText::new("Fast Rust-based Image Flasher").size(12.0).color(egui::Color32::GRAY));
                });
            });

            ui.add_space(20.0);

            ui.vertical(|ui| {
                ui.set_width(ui.available_width());

                // Image file selection
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("📁 Image File").size(16.0).strong());
                        ui.add_space(3.0);

                        ui.horizontal(|ui| {
                            let _response = ui.add_sized(
                                [ui.available_width() - 80.0, 25.0],
                                egui::TextEdit::singleline(&mut self.image_path)
                                    .hint_text("Select an image file...")
                            );

                            if ui.add_sized([75.0, 25.0], egui::Button::new("Browse")).clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("Image files", &["img", "iso", "gz", "zst"])
                                    .pick_file()
                                {
                                    self.image_path = path.display().to_string();
                                }
                            }
                        });
                    });
                });

                ui.add_space(10.0);

                // Device selection (multiple)
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("💾 Target Devices").size(16.0).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("🔄").on_hover_text("Refresh device list").clicked() {
                                    self.refresh_devices = true;
                                }
                            });
                        });
                        ui.add_space(3.0);

                        // Display selected devices
                        if !self.device_paths.is_empty() {
                            ui.label(egui::RichText::new(format!("Selected: {} device(s)", self.device_paths.len())).size(12.0).color(egui::Color32::GRAY));
                            ui.add_space(3.0);

                            let mut to_remove = None;

                            // Responsive wrapping grid
                            egui::ScrollArea::vertical()
                                .max_height(200.0)
                                .show(ui, |ui| {
                                    let available_width = ui.available_width();

                                    // Calculate layout parameters to fit items in the box
                                    let min_item_width = 150.0;
                                    let spacing = 8.0;
                                    let items_per_row = ((available_width + spacing) / (min_item_width + spacing)).floor().max(1.0) as usize;
                                    let actual_item_width = (available_width - (spacing * (items_per_row as f32 - 1.0))) / items_per_row as f32;

                                    // Layout items in rows
                                    for row_start in (0..self.device_paths.len()).step_by(items_per_row) {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = spacing;

                                            let row_end = (row_start + items_per_row).min(self.device_paths.len());
                                            for i in row_start..row_end {
                                                let device_path = &self.device_paths[i];

                                                ui.group(|ui| {
                                                    ui.set_max_width(actual_item_width);
                                                    ui.horizontal(|ui| {
                                                        ui.label("•");
                                                        ui.add(
                                                            egui::Label::new(device_path)
                                                                .truncate()
                                                        );
                                                        if ui.small_button("✖").on_hover_text("Remove this device").clicked() {
                                                            to_remove = Some(i);
                                                        }
                                                    });
                                                });
                                            }
                                        });
                                    }
                                });

                            if let Some(i) = to_remove {
                                self.device_paths.remove(i);
                                if i < self.selected_device_indices.len() {
                                    self.selected_device_indices.remove(i);
                                }
                            }
                            ui.add_space(5.0);
                        }

                        ui.horizontal(|ui| {
                            let selected_text = "Add device...".to_string();

                            egui::ComboBox::from_label("")
                                .selected_text(selected_text)
                                .width(ui.available_width() - 10.0)
                                .show_ui(ui, |ui| {
                                    for (i, device) in self.available_devices.iter().enumerate() {
                                        let display_name = device.display_name();
                                        let hover_text = format!(
                                            "Device: {}\nPath: {}\nSize: {}\nType: {}",
                                            device.name,
                                            device.path,
                                            device.size,
                                            device.device_type
                                        );

                                        // Check if already selected
                                        let already_selected = self.device_paths.contains(&device.path);
                                        let label = if already_selected {
                                            format!("✓ {}", display_name)
                                        } else {
                                            display_name
                                        };

                                        if ui.selectable_label(false, &label).on_hover_text(hover_text).clicked() {
                                            if !already_selected {
                                                self.device_paths.push(device.path.clone());
                                                self.selected_device_indices.push(i);
                                            }
                                        }
                                    }

                                    ui.separator();
                                    if ui.selectable_label(false, "Custom Path...").clicked() {
                                        self.device_paths.push(String::new());
                                    }
                                });
                        });

                        // Show custom path input if last device is empty
                        let should_show_custom = self.device_paths.last().map_or(false, |p| p.is_empty());
                        if should_show_custom {
                            ui.add_space(3.0);
                            let mut remove_last = false;
                            ui.horizontal(|ui| {
                                ui.label("Custom:");
                                if let Some(last) = self.device_paths.last_mut() {
                                    let response = ui.add_sized(
                                        [ui.available_width() - 60.0, 25.0],
                                        egui::TextEdit::singleline(last)
                                            .hint_text("e.g., /dev/sdb or /dev/disk2")
                                    );

                                    if ui.add_sized([55.0, 25.0], egui::Button::new("Add")).clicked() && !last.is_empty() {
                                        // Path is added, nothing more to do
                                    }

                                    if response.lost_focus() && last.is_empty() {
                                        // Remove empty entry if focus lost without input
                                        remove_last = true;
                                    }
                                }
                            });
                            if remove_last {
                                self.device_paths.pop();
                            }
                        }
                    });
                });

                ui.add_space(15.0);

                // Progress bar - Always displayed
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("⚡ Flashing Progress").size(16.0).strong());
                        ui.add_space(5.0);

                        let (progress_val, speed, elapsed) = if let Ok(progress_guard) = self.progress.lock() {
                            let progress_val = progress_guard.get_progress();
                            let speed = progress_guard.get_speed_bytes() / 1_048_576.0;
                            // Show current elapsed time during flashing, or stored time when completed
                            let elapsed = if self.flashing_state == FlashingState::InProgress {
                                progress_guard.get_elapsed_time().as_secs()
                            } else if let Some(completed) = self.completed_time {
                                completed
                            } else {
                                0
                            };
                            (progress_val, speed, elapsed)
                        } else {
                            (0.0, 0.0, 0)
                        };

                        ui.horizontal(|ui| {
                            ui.label(format!("Progress: {:.1}%", progress_val * 100.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(format!("{:.1} MB/s", speed));
                            });
                        });

                        ui.add_space(3.0);
                        ui.add(egui::ProgressBar::new(progress_val).show_percentage().desired_height(20.0));
                        ui.add_space(3.0);
                        ui.label(format!("Elapsed: {}s", elapsed));
                    });
                });
                ui.add_space(10.0);

                // Flash button
                ui.vertical_centered(|ui| {
                    let button_text = match self.flashing_state {
                        FlashingState::Idle => "🚀 Start Flashing",
                        FlashingState::InProgress => "⏳ Flashing...",
                        FlashingState::Completed => "✅ Flash Complete",
                        FlashingState::Error => "❌ Flash Failed",
                    };

                    let button_enabled = self.flashing_state == FlashingState::Idle &&
                                       !self.image_path.is_empty() &&
                                       !self.device_paths.is_empty();

                    ui.add_enabled_ui(button_enabled, |ui| {
                        if ui.add_sized([200.0, 40.0], egui::Button::new(
                            egui::RichText::new(button_text).size(16.0)
                        )).clicked() {
                            self.start_flashing();
                        }
                    });
                });

                ui.add_space(10.0);

                // Messages
                if let Some(error) = self.error_message {
                    ui.colored_label(egui::Color32::RED, format!("❌ {}", error));
                }

                if let Some(ref success) = self.success_message {
                    ui.colored_label(egui::Color32::GREEN, format!("✅ {}", success));
                }

                // Reset button for completed/error states
                if self.flashing_state == FlashingState::Completed || self.flashing_state == FlashingState::Error {
                    ui.vertical_centered(|ui| {
                        if ui.button("Flash Another").clicked() {
                            self.flashing_state = FlashingState::Idle;
                            self.error_message = None;
                            self.success_message = None;
                            self.completed_time = None; // Reset completion time
                        }
                    });
                }
            });
        });
    }
}

pub fn run_gui(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let state = State::new(args);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([500.0, 500.0])
            .with_min_inner_size([500.0, 500.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "ferrisflash 🦀",
        native_options,
        Box::new(|_cc| Ok(Box::new(state)))
    )?;

    Ok(())
}
