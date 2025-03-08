use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use flate2::read::GzDecoder;

pub struct Progress {
    bytes_written: u64,
    total_bytes: u64,
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
        self.bytes_written as f32 / self.total_bytes as f32
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

fn get_gzip_size<P: AsRef<Path>>(path: P) -> io::Result<u64> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::End(-4))?;
    let mut size_bytes = [0; 4];
    file.read_exact(&mut size_bytes)?;
    Ok(u32::from_le_bytes(size_bytes) as u64)
}

pub fn flash_image<P: AsRef<Path>>(
    image_path: P,
    device_path: P,
    progress: Arc<Mutex<Progress>>
) -> io::Result<()> {
    let image_file = File::open(&image_path)?;
    
    let total_size = if is_gzipped(&image_path)? {
        get_gzip_size(&image_path)?
    } else {
        image_file.metadata()?.len()
    };

    {
        let mut progress = progress.lock().unwrap();
        progress.total_bytes = total_size;
    }
    
    let device_file = File::create(device_path)?;
    let mut writer = BufWriter::with_capacity(1024 * 8192, device_file);
    
    let mut reader: Box<dyn Read> = if is_gzipped(&image_path)? {
        Box::new(BufReader::with_capacity(1024 * 8192, GzDecoder::new(image_file)))
    } else {
        Box::new(BufReader::with_capacity(1024 * 8192, image_file))
    };
    
    let mut buffer = vec![0; 1024 * 8192];
    let mut sync_data = 0u64;
    
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        
        writer.write_all(&buffer[..bytes_read])?;
        
        let mut progress = progress.lock().unwrap();
        progress.bytes_written += bytes_read as u64;
        sync_data += bytes_read as u64;

        if progress.bytes_written >= progress.total_bytes {
            break;
        }

        drop(progress);

        if sync_data >= 1024 * 1024 * 16 {
            writer.flush()?;
            writer.get_mut().sync_data()?;
            sync_data = 0;
        }
    }
    
    writer.flush()?;
    writer.get_mut().sync_all()?;
    drop(writer);

    Ok(())
}
