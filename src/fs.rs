use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::process::Command;
use flate2::read::GzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;

pub struct Progress {
    pub bytes_written: u64,
    pub total_bytes: u64,
    start_time: Instant,
}

impl Progress {
    pub fn new(total_bytes: u64) -> Self {
        Progress {
            bytes_written: 0,
            total_bytes,
            start_time: Instant::now(),
        }
    }

    pub fn get_elapsed_time(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn get_progress(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        let progress = self.bytes_written as f32 / self.total_bytes as f32;
        // clamp to 1.0 max to avoid overshooting 100%
        progress.min(1.0)
    }

    pub fn get_speed_bytes(&self) -> f32 {
        let elapsed = self.start_time.elapsed().as_secs_f32();
        if elapsed == 0.0 {
            return 0.0;
        }

        self.bytes_written as f32 / elapsed
    }
}

fn is_gzipped<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0; 2];
    file.read_exact(&mut magic)?;
    Ok(magic == [0x1f, 0x8b])
}

fn is_zstd<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;

    // zstd magic number is 0xFD2FB528 (little endian) or 0x28B52FFD (big endian)
    Ok(magic[0] == 0x28 && magic[1] == 0xB5 && magic[2] == 0x2F && magic[3] == 0xFD)
}


fn get_img_size_from_header(header_buffer: &[u8]) -> u64 {
    if header_buffer.len() < 512 {
        return 0;
    }

    // check for MBR
    if header_buffer.len() >= 512 {
        // MBR signature 0x55AA at offset 510-511
        if header_buffer[510] == 0x55 && header_buffer[511] == 0xAA {
            let mut max_end_sector = 0u32;

            // parse partition entries (4 entries, 16 bytes each, starting at offset 446)
            for i in 0..4 {
                let offset = 446 + (i * 16);
                if offset + 16 <= header_buffer.len() {
                    // LBA start (4 bytes at offset 8)
                    let lba_start = u32::from_le_bytes([
                        header_buffer[offset + 8],
                        header_buffer[offset + 9],
                        header_buffer[offset + 10],
                        header_buffer[offset + 11],
                    ]);

                    // Sector count (4 bytes at offset 12)
                    let sector_count = u32::from_le_bytes([
                        header_buffer[offset + 12],
                        header_buffer[offset + 13],
                        header_buffer[offset + 14],
                        header_buffer[offset + 15],
                    ]);

                    if lba_start > 0 && sector_count > 0 {
                        let end_sector = lba_start + sector_count;
                        max_end_sector = max_end_sector.max(end_sector);
                    }
                }
            }

            if max_end_sector > 0 {
                return max_end_sector as u64 * 512;
            }
        }
    }

    // check for GPT
    if header_buffer.len() >= 1024 {
        // GPT header starts at LBA 1 (offset 512)
        let gpt_offset = 512;

        if gpt_offset + 8 <= header_buffer.len() &&
           &header_buffer[gpt_offset..gpt_offset + 8] == b"EFI PART" {
            // backup LBA is at offset 32-39 in GPT header (8 bytes, little endian)
            let backup_lba_offset = gpt_offset + 32;
            if backup_lba_offset + 8 <= header_buffer.len() {
                let backup_lba = u64::from_le_bytes([
                    header_buffer[backup_lba_offset],
                    header_buffer[backup_lba_offset + 1],
                    header_buffer[backup_lba_offset + 2],
                    header_buffer[backup_lba_offset + 3],
                    header_buffer[backup_lba_offset + 4],
                    header_buffer[backup_lba_offset + 5],
                    header_buffer[backup_lba_offset + 6],
                    header_buffer[backup_lba_offset + 7],
                ]);

                // GPT backup LBA is the last usable LBA, so size is (backup_lba + 1) * 512
                if backup_lba > 0 {
                    return (backup_lba + 1) * 512;
                }
            }
        }
    }

    // unable to determine size from header
    0
}

fn get_file_info<P: AsRef<Path>>(path: P) -> io::Result<(u64, bool)> {
    if is_gzipped(&path)? || is_zstd(&path)? {
        // determine size during decompression
        return Ok((0, true));
    }

    // For uncompressed files, just use the file size
    let file = File::open(&path)?;
    let size = file.metadata()?.len();
    Ok((size, false))
}

pub fn flash_images<P: AsRef<Path>, Q: AsRef<Path>>(
    image_path: P,
    device_paths: Vec<Q>,
    progress: Arc<Mutex<Progress>>
) -> io::Result<()> {
    if device_paths.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "No device paths provided"));
    }

    // Create writers for all devices
    let mut writers: Vec<BufWriter<File>> = Vec::new();
    for device_path in &device_paths {
        let device_file = File::create(device_path)?;
        writers.push(BufWriter::with_capacity(1024 * 8192, device_file));
    }

    let (total_size, is_compressed) = get_file_info(&image_path)?;

    {
        let mut progress = progress.lock().unwrap();
        progress.total_bytes = total_size;
    }

    let file = File::open(&image_path)?;
    let mut reader: Box<dyn Read> = create_reader(&image_path, file)?;

    if is_compressed {
        flash_data_with_header_detection_multi(&mut reader, &mut writers, progress)?;
    } else {
        flash_data_multi(&mut reader, &mut writers, progress)?;
    }

    // Flush and sync all writers
    for writer in &mut writers {
        writer.flush()?;
        writer.get_mut().sync_all()?;
    }

    Ok(())
}

fn create_reader<P: AsRef<Path>>(image_path: P, file: File) -> io::Result<Box<dyn Read>> {
    if is_gzipped(&image_path)? {
        Ok(Box::new(BufReader::with_capacity(1024 * 8192, GzDecoder::new(file))))
    } else if is_zstd(&image_path)? {
        let decoder = ZstdDecoder::new(file)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        Ok(Box::new(BufReader::with_capacity(1024 * 8192, decoder)))
    } else {
        Ok(Box::new(BufReader::with_capacity(1024 * 8192, file)))
    }
}

fn write_buffer_chunk_multi(writers: &mut [BufWriter<File>], chunk: &[u8]) -> io::Result<()> {
    let is_all_zeros = chunk.iter().all(|&b| b == 0);

    for writer in writers.iter_mut() {
        if is_all_zeros {
            // For all-zero blocks, seek forward instead of writing
            writer.seek(SeekFrom::Current(chunk.len() as i64))?;
            writer.flush()?;
        } else {
            writer.write_all(chunk)?;
        }
    }
    Ok(())
}

fn flash_data_multi(
    reader: &mut Box<dyn Read>,
    writers: &mut [BufWriter<File>],
    progress: Arc<Mutex<Progress>>,
) -> io::Result<()> {
    let mut buffer = vec![0; 1024 * 1024]; // 1MB buffer
    let mut sync_data = 0u64;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        write_buffer_chunk_multi(writers, &buffer[..bytes_read])?;

        {
            let mut progress = progress.lock().unwrap();
            progress.bytes_written += bytes_read as u64;
        }

        sync_data += bytes_read as u64;
        if sync_data >= 1024 * 1024 * 32 {
            for writer in writers.iter_mut() {
                writer.flush()?;
                writer.get_mut().sync_data()?;
            }
            sync_data = 0;
        }
    }

    Ok(())
}

fn flash_data_with_header_detection_multi(
    reader: &mut Box<dyn Read>,
    writers: &mut [BufWriter<File>],
    progress: Arc<Mutex<Progress>>,
) -> io::Result<()> {
    let mut buffer = vec![0; 1024 * 1024]; // 1MB buffer
    let mut sync_data = 0u64;
    let mut total_written = 0u64;
    let mut header_buffer = Vec::new();
    let mut size_determined = false;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        // Accumulate header data until we can determine the size or reach 64KB
        if !size_determined && header_buffer.len() < 65536 {
            let bytes_to_add = (65536 - header_buffer.len()).min(bytes_read);
            header_buffer.extend_from_slice(&buffer[..bytes_to_add]);

            // 1024 bytes should be enough to check for MBR/GPT
            if header_buffer.len() >= 1024 {
                let img_size = get_img_size_from_header(&header_buffer);
                if img_size > 0 {
                    {
                        let mut progress = progress.lock().unwrap();
                        progress.total_bytes = img_size;
                    }
                    size_determined = true;
                }
            }
        }

        write_buffer_chunk_multi(writers, &buffer[..bytes_read])?;
        total_written += bytes_read as u64;

        {
            let mut progress = progress.lock().unwrap();
            progress.bytes_written = total_written;

            // If we haven't determined the size yet, use streaming-style progress
            if !size_determined {
                progress.total_bytes = total_written + (total_written / 4).max(1024 * 1024);
            }
        }

        sync_data += bytes_read as u64;
        if sync_data >= 1024 * 1024 * 32 { // 32MB
            for writer in writers.iter_mut() {
                writer.flush()?;
                writer.get_mut().sync_data()?;
            }
            sync_data = 0;
        }
    }

    {
        let mut progress = progress.lock().unwrap();
        if !size_determined {
            progress.total_bytes = total_written;
        }
        progress.bytes_written = total_written;
    }

    Ok(())
}



#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub path: String,
    pub name: String,
    pub size: String,
    pub device_type: String,
}

impl DeviceInfo {
    pub fn display_name(&self) -> String {
        if self.name.is_empty() || self.name == "Unknown Device" {
            if self.size == "Unknown" {
                format!("{}", self.path)
            } else {
                format!("{} ({})", self.path, self.size)
            }
        } else {
            if self.size == "Unknown" {
                format!("{} - {}", self.name, self.path)
            } else {
                format!("{} ({}) - {}", self.name, self.size, self.path)
            }
        }
    }
}

pub fn enumerate_devices() -> Vec<DeviceInfo> {
    #[cfg(target_os = "linux")]
    {
        enumerate_linux_devices()
    }
    #[cfg(target_os = "macos")]
    {
        enumerate_macos_devices()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn enumerate_linux_devices() -> Vec<DeviceInfo> {
    if let Some(devices) = try_enumerate_with_lsblk() {
        return devices;
    }

    // fallback: scan /dev for common device patterns
    enumerate_fallback_devices()
}

#[cfg(target_os = "linux")]
fn try_enumerate_with_lsblk() -> Option<Vec<DeviceInfo>> {
    let output = Command::new("lsblk")
        .args(&["-J", "-o", "NAME,SIZE,TYPE,MODEL,MOUNTPOINT,VENDOR,SERIAL,HOTPLUG,RM"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json_str = String::from_utf8(output.stdout).ok()?;
    let json: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let blockdevices = json["blockdevices"].as_array()?;

    let mut devices = Vec::new();
    for device in blockdevices {
        if let Some(device_info) = parse_lsblk_device(device) {
            devices.push(device_info);
        }
    }

    Some(devices)
}

#[cfg(target_os = "linux")]
fn parse_lsblk_device(device: &serde_json::Value) -> Option<DeviceInfo> {
    let name = device["name"].as_str()?;
    let size = device["size"].as_str()?;
    let device_type = device["type"].as_str()?;

    if device_type != "disk" {
        return None;
    }

    let mountpoint = device["mountpoint"].as_str().unwrap_or("");
    let model = device["model"].as_str().unwrap_or("");
    let vendor = device["vendor"].as_str().unwrap_or("");
    let hotplug = device["hotplug"].as_str().unwrap_or("0");
    let removable = device["rm"].as_str().unwrap_or("0");

    let removable_path = format!("/sys/block/{}/removable", name);
    let is_removable = removable == "1" ||
                      hotplug == "1" ||
                      std::fs::read_to_string(&removable_path)
                          .map(|s| s.trim() == "1")
                          .unwrap_or(false);

    let should_include = is_removable ||
                        mountpoint.is_empty() ||
                        name.starts_with("sd") ||
                        name.starts_with("mmcblk");

    if !should_include {
        return None;
    }

    let device_name = build_device_name(vendor, model, name, is_removable);

    Some(DeviceInfo {
        path: format!("/dev/{}", name),
        name: device_name,
        size: format_size(size),
        device_type: if is_removable { "Removable" } else { "Disk" }.to_string(),
    })
}

#[cfg(target_os = "linux")]
fn build_device_name(vendor: &str, model: &str, name: &str, is_removable: bool) -> String {
    let mut parts = Vec::new();

    if !vendor.is_empty() && vendor != "ATA" {
        parts.push(vendor.trim());
    }

    if !model.is_empty() {
        parts.push(model.trim());
    }

    if parts.is_empty() {
        if is_removable {
            if name.starts_with("mmcblk") {
                "SD Card".to_string()
            } else {
                "USB Drive".to_string()
            }
        } else {
            "Unknown Device".to_string()
        }
    } else {
        parts.join(" ")
    }
}

#[cfg(target_os = "linux")]
fn format_size(size: &str) -> String {
    // lsblk already gives us human-readable sizes, but let's ensure consistency
    if size.is_empty() {
        "Unknown".to_string()
    } else {
        size.to_string()
    }
}

#[cfg(target_os = "linux")]
fn enumerate_fallback_devices() -> Vec<DeviceInfo> {
    let dev_dir = std::fs::read_dir("/dev").unwrap_or_else(|_| std::fs::read_dir(".").unwrap());

    dev_dir
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();

            if (name.starts_with("sd") && name.len() == 3) ||
               name.starts_with("mmcblk") {

                let device_name = if name.starts_with("mmcblk") {
                    "SD Card"
                } else {
                    "USB Drive"
                };

                let size = get_device_size_from_sys(&name);

                Some(DeviceInfo {
                    path: format!("/dev/{}", name),
                    name: device_name.to_string(),
                    size,
                    device_type: "Removable".to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn get_device_size_from_sys(device_name: &str) -> String {
    let size_path = format!("/sys/block/{}/size", device_name);

    if let Ok(size_sectors) = std::fs::read_to_string(&size_path) {
        if let Ok(sectors) = size_sectors.trim().parse::<u64>() {
            let bytes = sectors * 512;
            return format_bytes_to_human_readable(bytes);
        }
    }

    "Unknown".to_string()
}

#[cfg(target_os = "linux")]
fn format_bytes_to_human_readable(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    const THRESHOLD: u64 = 1024;

    if bytes == 0 {
        return "0 B".to_string();
    }

    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= THRESHOLD as f64 && unit_index < UNITS.len() - 1 {
        size /= THRESHOLD as f64;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

#[cfg(target_os = "macos")]
fn enumerate_macos_devices() -> Vec<DeviceInfo> {
    let output = match Command::new("diskutil").args(&["list"]).output() {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };

    let output_str = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    output_str
        .lines()
        .filter(|line| line.contains("/dev/disk") &&
                      (line.contains("external") || line.contains("GUID_partition_scheme")))
        .filter_map(|line| line.split_whitespace().last())
        .filter(|disk_name| disk_name.starts_with("/dev/disk"))
        .filter_map(|disk_name| get_macos_device_info(disk_name))
        .collect()
}

#[cfg(target_os = "macos")]
fn get_macos_device_info(disk_name: &str) -> Option<DeviceInfo> {
    let info_output = Command::new("diskutil")
        .args(&["info", disk_name])
        .output()
        .ok()?;

    let info_str = String::from_utf8(info_output.stdout).ok()?;

    let mut device_name = "Unknown Device".to_string();
    let mut media_name = String::new();
    let mut size = "Unknown".to_string();
    let mut is_removable = false;
    let mut is_external = false;

    for info_line in info_str.lines() {
        let line = info_line.trim();

        if line.starts_with("Device / Media Name:") {
            if let Some(name) = line.split(':').nth(1) {
                media_name = name.trim().to_string();
            }
        } else if line.starts_with("Device Identifier:") {
            if let Some(identifier) = line.split(':').nth(1) {
                if identifier.trim() != disk_name {
                    device_name = identifier.trim().to_string();
                }
            }
        } else if line.starts_with("Disk Size:") {
            if let Some(disk_size) = line.split(':').nth(1) {
                let size_parts: Vec<&str> = disk_size.trim().split_whitespace().collect();
                if size_parts.len() >= 2 {
                    size = format!("{} {}", size_parts[0], size_parts[1]);
                }
            }
        } else if line.starts_with("Removable Media:") {
            is_removable = line.contains("Yes");
        } else if line.starts_with("Protocol:") {
            let protocol = line.split(':').nth(1).unwrap_or("").trim().to_lowercase();
            is_external = protocol.contains("usb") || protocol.contains("firewire") || protocol.contains("thunderbolt");
        } else if line.starts_with("Physical Interconnect:") {
            let interconnect = line.split(':').nth(1).unwrap_or("").trim().to_lowercase();
            is_external = interconnect.contains("usb") || interconnect.contains("firewire") || interconnect.contains("thunderbolt");
        }
    }

    if !is_removable && !is_external {
        return None;
    }

    let final_name = if !media_name.is_empty() && media_name != "Unknown Device" {
        media_name
    } else {
        build_macos_device_name(disk_name, is_external)
    };

    Some(DeviceInfo {
        path: disk_name.to_string(),
        name: final_name,
        size,
        device_type: if is_removable { "Removable" } else { "External" }.to_string(),
    })
}

#[cfg(target_os = "macos")]
fn build_macos_device_name(disk_name: &str, is_external: bool) -> String {
    if disk_name.contains("disk") {
        if is_external {
            "External Drive".to_string()
        } else {
            "USB Drive".to_string()
        }
    } else {
        "Unknown Device".to_string()
    }
}
